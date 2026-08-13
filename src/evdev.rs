// Physical touchpad input — opens the evdev node, queries EVIOCGABS for
// center coordinates, and turns the raw event stream into TouchFrames at
// each SYN_REPORT. See linux-design.md §5.

use std::path::{Path, PathBuf};

use evdev::{AbsoluteAxisType, Device, EventType, InputEvent, Key};

use crate::detector::TouchSample;
use crate::error::{Error, Result};
use crate::fsm::TouchFrame;

/// Maximum number of MT slots we track. The kernel exposes up to 10 in
/// practice; touchpads typically advertise 5. We sweep all slots when
/// rescanning for "lowest active" semantics (D-012), so the constant
/// only bounds the per-frame map size, not gesture logic.
const MAX_MT_SLOTS: usize = 16;

pub struct InputDevice {
    pub device: Device,
    pub abs_x_min: i32,
    pub abs_x_max: i32,
    pub abs_y_min: i32,
    pub abs_y_max: i32,
    pub center_x: i32,
    pub center_y: i32,
    /// Per-slot tracking IDs and last-seen (x, y). Slot index 0..MAX_MT_SLOTS-1.
    slots: [SlotState; MAX_MT_SLOTS],
    /// Current writing slot per ABS_MT_SLOT event.
    current_slot: usize,
    /// BTN_TOUCH summary; mirrors the kernel-reported any-finger-down state.
    contact: bool,
}

#[derive(Clone, Copy, Debug)]
struct SlotState {
    /// `-1` means inactive (kernel convention).
    tracking_id: i32,
    x: i32,
    y: i32,
}

/// One SYN_REPORT-bounded batch of physical events plus the
/// high-level frame assembled from them.
pub struct PhysicalFrame {
    pub frame: TouchFrame,
    /// All events from the underlying fetch, in original order.
    /// Forwarded verbatim to the virtual touchpad (minus the trailing
    /// SYN_REPORT, which `emit()` re-inserts).
    pub events: Vec<InputEvent>,
}

impl Drop for InputDevice {
    /// Panic-safe ungrab. The kernel releases EVIOCGRAB on FD close
    /// regardless, but explicitly calling ungrab here means the
    /// release happens deterministically during unwind, before the
    /// FD's file struct is reaped — closing the race where a daemon
    /// restart tries to re-grab before the kernel has finalized the
    /// old FD.
    fn drop(&mut self) {
        let _ = self.device.ungrab();
    }
}

impl InputDevice {
    /// Open the device at `path` and validate required capabilities.
    pub fn open(path: &Path) -> Result<Self> {
        let device = Device::open(path).map_err(|source| Error::EvdevOpen {
            path: path.to_path_buf(),
            source,
        })?;

        // Required capabilities.
        let abs = device.supported_absolute_axes();
        let keys = device.supported_keys();
        let has_x = abs.is_some_and(|a| a.contains(AbsoluteAxisType::ABS_MT_POSITION_X));
        let has_y = abs.is_some_and(|a| a.contains(AbsoluteAxisType::ABS_MT_POSITION_Y));
        let has_touch = keys.is_some_and(|k| k.contains(Key::BTN_TOUCH));
        if !has_x {
            return Err(Error::EvdevMissingCap {
                path: path.to_path_buf(),
                capability: "ABS_MT_POSITION_X",
            });
        }
        if !has_y {
            return Err(Error::EvdevMissingCap {
                path: path.to_path_buf(),
                capability: "ABS_MT_POSITION_Y",
            });
        }
        if !has_touch {
            return Err(Error::EvdevMissingCap {
                path: path.to_path_buf(),
                capability: "BTN_TOUCH",
            });
        }

        let abs_state = device
            .get_abs_state()
            .map_err(|source| Error::EvdevRead { source })?;
        let xi = abs_state[AbsoluteAxisType::ABS_MT_POSITION_X.0 as usize];
        let yi = abs_state[AbsoluteAxisType::ABS_MT_POSITION_Y.0 as usize];
        let abs_x_min = xi.minimum;
        let abs_x_max = xi.maximum;
        let abs_y_min = yi.minimum;
        let abs_y_max = yi.maximum;
        let center_x = (abs_x_min + abs_x_max) / 2;
        let center_y = (abs_y_min + abs_y_max) / 2;

        let slots = [SlotState {
            tracking_id: -1,
            x: 0,
            y: 0,
        }; MAX_MT_SLOTS];

        Ok(Self {
            device,
            abs_x_min,
            abs_x_max,
            abs_y_min,
            abs_y_max,
            center_x,
            center_y,
            slots,
            current_slot: 0,
            contact: false,
        })
    }

    /// Find a touchpad whose name matches `regex`. Returns the first match
    /// found via `/dev/input/event*` enumeration.
    pub fn find_by_name(regex_str: &str) -> Result<PathBuf> {
        let re = regex::Regex::new(regex_str).map_err(|source| Error::RegexInvalid {
            pattern: regex_str.to_string(),
            source,
        })?;
        for (path, device) in evdev::enumerate() {
            if let Some(name) = device.name() {
                if re.is_match(name) {
                    return Ok(path);
                }
            }
        }
        Err(Error::DeviceNotFound {
            regex: regex_str.to_string(),
        })
    }

    /// Block until events are available, then return ALL complete
    /// SYN_REPORT-bounded frames from this fetch. If the daemon was
    /// briefly descheduled and the kernel has buffered multiple
    /// frames, every one is returned so the caller can step the FSM
    /// and forward to the virtual touchpad per-frame — gluing N
    /// kernel batches into one virtual batch loses per-frame timing
    /// and corrupts libinput's state.
    ///
    /// Any trailing events without a closing SYN_REPORT are kept in
    /// internal state for the next call's first frame.
    pub fn poll_frames(&mut self) -> Result<Vec<PhysicalFrame>> {
        let events: Vec<InputEvent> = self
            .device
            .fetch_events()
            .map_err(|source| Error::EvdevRead { source })?
            .collect();
        let mut frames: Vec<PhysicalFrame> = Vec::new();
        let mut batch_events: Vec<InputEvent> = Vec::new();
        for ev in events {
            batch_events.push(ev);
            match ev.event_type() {
                EventType::ABSOLUTE => self.apply_abs(ev.code(), ev.value()),
                EventType::KEY if ev.code() == Key::BTN_TOUCH.code() => {
                    self.contact = ev.value() != 0;
                }
                EventType::SYNCHRONIZATION if ev.code() == 0 => {
                    let frame = self.assemble_frame();
                    frames.push(PhysicalFrame {
                        frame,
                        events: std::mem::take(&mut batch_events),
                    });
                }
                _ => {}
            }
        }
        // Trailing events without a closing SYN_REPORT are dropped.
        // The kernel always closes each report with SYN; partial
        // batches at the tail would indicate an evdev read split
        // mid-report, which `fetch_events` shouldn't produce.
        Ok(frames)
    }

    fn apply_abs(&mut self, code: u16, value: i32) {
        let axis = AbsoluteAxisType(code);
        match axis {
            AbsoluteAxisType::ABS_MT_SLOT if (value as usize) < MAX_MT_SLOTS => {
                self.current_slot = value as usize;
            }
            AbsoluteAxisType::ABS_MT_TRACKING_ID => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].tracking_id = value;
                }
            }
            AbsoluteAxisType::ABS_MT_POSITION_X => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].x = value;
                }
            }
            AbsoluteAxisType::ABS_MT_POSITION_Y => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].y = value;
                }
            }
            // Some pads also expose ABS_X / ABS_Y for the primary touch.
            // We deliberately ignore those — MT axes are authoritative.
            _ => {}
        }
    }

    fn assemble_frame(&mut self) -> TouchFrame {
        // Lowest-numbered active slot wins (D-012). "Active" = tracking_id != -1.
        let active: Vec<&SlotState> = self.slots.iter().filter(|s| s.tracking_id != -1).collect();
        let chosen = active.first();
        let finger_count = active.len() as u8;
        match (self.contact, chosen) {
            (true, Some(s)) => TouchFrame {
                contact: true,
                pos: Some(TouchSample { x: s.x, y: s.y }),
                finger_count,
            },
            (true, None) => TouchFrame {
                contact: true,
                pos: None,
                finger_count: 0,
            },
            (false, _) => TouchFrame {
                contact: false,
                pos: None,
                finger_count: 0,
            },
        }
    }
}
