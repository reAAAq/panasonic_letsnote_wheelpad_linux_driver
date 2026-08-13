// FSM synthetic tests — see linux-design.md §13.

use std::f64::consts::PI;

use letsnote_wheelpad::config::Scroll;
use letsnote_wheelpad::detector::{CircularDetector, TouchSample};
use letsnote_wheelpad::fsm::{Action, Fsm, FsmState, TouchFrame};

fn default_scroll() -> Scroll {
    Scroll::default()
}

fn lift() -> TouchFrame {
    TouchFrame {
        contact: false,
        pos: None,
    }
}

fn touch(x: i32, y: i32) -> TouchFrame {
    TouchFrame {
        contact: true,
        pos: Some(TouchSample { x, y }),
    }
}

fn drive(
    fsm: &mut Fsm,
    detector: &mut CircularDetector,
    scroll: &Scroll,
    frames: &[TouchFrame],
) -> Vec<Action> {
    let mut acc = Vec::new();
    for f in frames {
        let action = fsm.step(*f, detector, scroll);
        if !matches!(action, Action::None) {
            acc.push(action);
        }
    }
    acc
}

#[test]
fn idle_to_contact_on_touchdown_inside_dead_zone() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    let _ = drive(&mut fsm, &mut det, &scroll, &[touch(510, 510)]);
    assert!(matches!(fsm.state(), FsmState::Contact { .. }));
}

#[test]
fn idle_to_moving_on_touchdown_outside_dead_zone() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    let _ = drive(&mut fsm, &mut det, &scroll, &[touch(720, 500)]); // r = 220
    assert!(matches!(fsm.state(), FsmState::Moving { .. }));
}

#[test]
fn contact_to_idle_on_lift() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    drive(&mut fsm, &mut det, &scroll, &[touch(510, 510), lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn contact_does_not_engage_on_cross_gate_movement_d020() {
    // D-020: strict Windows dead-zone semantics. Once trapped in
    // Contact, even sliding outside the gate does not engage Moving.
    // The user must lift and re-touch.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    // Touch inside dead zone, then slide outside.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(510, 510), touch(550, 500), touch(720, 500)],
    );
    assert!(
        matches!(fsm.state(), FsmState::Contact { .. }),
        "expected to stay in Contact, got {:?}",
        fsm.state()
    );
}

#[test]
fn moving_to_idle_on_lift_before_engagement() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    drive(&mut fsm, &mut det, &scroll, &[touch(720, 500), lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn moving_to_contact_on_slip_back_into_dead_zone() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    // Engage outside, then slip back inside.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(720, 500), touch(550, 500)],
    );
    assert!(matches!(fsm.state(), FsmState::Contact { .. }));
}

#[test]
fn moving_to_scrolling_on_swept_angle_past_trigger() {
    // Sweep past the trigger angle (10°) from engage_start while staying
    // outside the radial gate → Scrolling. With the passthrough
    // architecture there is no longer a Grab action to observe; the
    // state transition is the signal the runtime keys off.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    // engage_start at angle 0, r=220
    let start = touch(720, 500);
    // sweep to angle π/8 (= 22.5°, > trigger 10°), r=220
    let theta = PI / 8.0;
    let end_x = 500 + (220.0 * theta.cos()).round() as i32;
    let end_y = 500 + (220.0 * theta.sin()).round() as i32;
    let end = touch(end_x, end_y);

    drive(&mut fsm, &mut det, &scroll, &[start, end]);
    assert!(matches!(fsm.state(), FsmState::Scrolling));
}

#[test]
fn scrolling_to_debounce_on_lift() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let theta = PI / 8.0;
    let mid_x = 500 + (220.0 * theta.cos()).round() as i32;
    let mid_y = 500 + (220.0 * theta.sin()).round() as i32;
    let mid = touch(mid_x, mid_y);

    drive(&mut fsm, &mut det, &scroll, &[start, mid]);
    assert!(matches!(fsm.state(), FsmState::Scrolling));

    drive(&mut fsm, &mut det, &scroll, &[lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));
}

#[test]
fn force_idle_resets_state() {
    // Watchdog path: after force_idle the FSM is back at Idle even if
    // it was mid-Scrolling.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let theta = PI / 8.0;
    let mid = touch(
        500 + (220.0 * theta.cos()).round() as i32,
        500 + (220.0 * theta.sin()).round() as i32,
    );
    drive(&mut fsm, &mut det, &scroll, &[start, mid]);
    assert!(matches!(fsm.state(), FsmState::Scrolling));

    fsm.force_idle(&mut det);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn debounce_to_idle_on_next_frame_no_timer() {
    // D-011-followup: Debounce always exits to Idle on the next frame
    // regardless of whether the finger is now down or up.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let theta = PI / 8.0;
    let mid_x = 500 + (220.0 * theta.cos()).round() as i32;
    let mid_y = 500 + (220.0 * theta.sin()).round() as i32;
    let mid = touch(mid_x, mid_y);

    drive(&mut fsm, &mut det, &scroll, &[start, mid, lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));

    drive(&mut fsm, &mut det, &scroll, &[lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn debounce_to_idle_even_if_finger_back_down() {
    // Per D-011-followup, the Debounce state has no re-engagement
    // path. If a finger is down on the very next frame, we still go to
    // Idle this frame; the frame *after* that re-runs the fresh-touch
    // classifier.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let theta = PI / 8.0;
    let mid_x = 500 + (220.0 * theta.cos()).round() as i32;
    let mid_y = 500 + (220.0 * theta.sin()).round() as i32;
    let mid = touch(mid_x, mid_y);

    drive(&mut fsm, &mut det, &scroll, &[start, mid, lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));

    drive(&mut fsm, &mut det, &scroll, &[touch(720, 500)]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn disabled_scroll_holds_idle() {
    // D-007: when scroll.enable = false the daemon keeps reading
    // frames but the FSM never advances past Idle.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let mut scroll = default_scroll();
    scroll.enable = false;
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(720, 500), touch(700, 520), lift()],
    );
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn scrolling_exits_to_moving_on_stationary_finger() {
    // A finger that stops moving while Scrolling must drop back to
    // Moving so the passthrough runtime resumes forwarding and the
    // cursor unfreezes — instead of waiting for a lift or the 5 s
    // watchdog.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let theta = PI / 8.0;
    let mid = touch(
        500 + (220.0 * theta.cos()).round() as i32,
        500 + (220.0 * theta.sin()).round() as i32,
    );
    drive(&mut fsm, &mut det, &scroll, &[start, mid]);
    assert!(matches!(fsm.state(), FsmState::Scrolling));

    // Finger rests at `mid`: after the stationary threshold it must
    // drop back to Moving.
    drive(&mut fsm, &mut det, &scroll, &vec![mid; 40]);
    assert!(
        matches!(fsm.state(), FsmState::Moving { .. }),
        "expected Moving after stationary finger, got {:?}",
        fsm.state()
    );
}

#[test]
fn stationary_exit_does_not_trip_during_continuous_circle() {
    // Guard: a continuous circle moves every frame well past the dead
    // band, so the stationary counter must never reach the exit
    // threshold and the gesture must stay Scrolling throughout.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500); // 0°
    let mut frames = vec![start];
    for i in 1..=72 {
        let t = i as f64 * (PI / 18.0); // 10° per step
        frames.push(touch(
            500 + (220.0 * t.cos()).round() as i32,
            500 + (220.0 * t.sin()).round() as i32,
        ));
    }
    drive(&mut fsm, &mut det, &scroll, &frames);
    assert!(
        matches!(fsm.state(), FsmState::Scrolling),
        "continuous circle must stay Scrolling, got {:?}",
        fsm.state()
    );
}

#[test]
fn scrolling_survives_position_gap() {
    // MT protocol race: a frame with contact=true but pos=None (slot
    // tracking id momentarily missing) must NOT exit Scrolling — the
    // finger is still down.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let theta = PI / 8.0;
    let mid = touch(
        500 + (220.0 * theta.cos()).round() as i32,
        500 + (220.0 * theta.sin()).round() as i32,
    );
    drive(&mut fsm, &mut det, &scroll, &[start, mid]);
    assert!(matches!(fsm.state(), FsmState::Scrolling));

    let gap = TouchFrame {
        contact: true,
        pos: None,
    };
    drive(&mut fsm, &mut det, &scroll, &[gap]);
    assert!(
        matches!(fsm.state(), FsmState::Scrolling),
        "contact=true with pos=None must stay Scrolling, got {:?}",
        fsm.state()
    );
}
