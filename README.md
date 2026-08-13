# letsnote-wheelpad

> English version: see [README.en.md](README.en.md).

一个用户态 Linux 守护进程，用于复现松下 Let's Note「WheelPad」的圆形触摸板滚动行为。在触摸板外圈缓慢画圆即可垂直滚动——和 Windows 上一样。

通过直接读取物理 Synaptics 触摸板的 evdev 事件，并通过 `uinput` 虚拟设备发出滚轮事件，因此在 Wayland 和 X11 上都能工作。物理触摸板照常驱动光标；本守护进程只额外提供滚动。

## 为什么需要它

`libinput` 拒绝在 Wayland 时代加入圆形滚动（参见 Peter Hutterer 2015 年的讨论）。所以如果你想让 Let's Note 的圆形滚动在 Linux 上工作，唯一的办法就是一个用户态守护进程：通过 evdev 读取触摸板，并通过单独的虚拟设备发出滚轮事件。本项目正是如此。

## 安装

### Ubuntu / Debian

```sh
sudo dpkg -i letsnote-wheelpad_0.1.0_amd64.deb
systemctl --user enable --now letsnote-wheelpad.service
```

### Fedora / RHEL

```sh
sudo rpm -i letsnote-wheelpad-0.1.0-1.x86_64.rpm
systemctl --user enable --now letsnote-wheelpad.service
```

### Arch

```sh
yay -S letsnote-wheelpad      # AUR
systemctl --user enable --now letsnote-wheelpad.service
```

### 从源码安装

```sh
git clone https://github.com/reAAAq/panasonic_letsnote_wheelpad_linux_driver
cd letsnote-wheelpad
cargo build --release
sudo install -Dm755 target/release/letsnote-wheelpad /usr/bin/letsnote-wheelpad
sudo install -Dm644 packaging/udev/70-letsnote-wheelpad.rules /etc/udev/rules.d/70-letsnote-wheelpad.rules
sudo install -Dm644 packaging/systemd/letsnote-wheelpad.service /etc/systemd/user/letsnote-wheelpad.service
sudo install -Dm644 packaging/modules-load/letsnote-wheelpad.conf /etc/modules-load.d/letsnote-wheelpad.conf
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo modprobe uinput
systemctl --user daemon-reload
systemctl --user enable --now letsnote-wheelpad.service
```

## 配置

配置文件位于 `~/.config/letsnote-wheelpad/config.toml`。所有键都是可选的；默认值与 Windows 开箱行为一致。

```toml
# 按名称正则自动检测。仅当触摸板非标准时才需要覆盖。
# device = "/dev/input/event4"
# device_name_regex = "Synaptics.*TM3562"

[scroll]
enable               = true   # 总开关
reverse_vertical     = false  # 翻转垂直滚动方向
horizontal_enable    = false  # 启用底部边缘水平滚动扇区
reverse_horizontal   = false
sensitivity          = 0      # -2..+2 ；越小越不灵敏
detect_area_width    = 0      # 0..10 ；0 = 仅外圈，10 = 整个触摸板
horizontal_start     = 2      # 弧起点，单位 π/8（2 → 45°）
horizontal_end       = 6      # 弧终点，单位 π/8（6 → 135°）

[log]
level = "info"  # trace | debug | info | warn | error
```

| 键 | 默认值 | 范围 | 说明 |
| --- | --- | --- | --- |
| `scroll.enable` | `true` | bool | 关闭后守护进程保持运行，但不再产生任何滚动。 |
| `scroll.reverse_vertical` | `false` | bool | 「自然」滚动 = `true`。 |
| `scroll.horizontal_enable` | `false` | bool | 默认关闭；与 Windows 一致。 |
| `scroll.reverse_horizontal` | `false` | bool | |
| `scroll.sensitivity` | `0` | -2..+2 | 索引倍率表 `[10, 14, 20, 28, 40]`。 |
| `scroll.detect_area_width` | `0` | 0..10 | `0` = 要求手指靠近边缘；`10` = 整个触摸板。 |
| `scroll.horizontal_start` | `2` | 0..15 | 单位 π/8。默认 45° → 135° = 触摸板底边。 |
| `scroll.horizontal_end` | `6` | 0..15 | |

### 查看日志

```sh
journalctl --user -u letsnote-wheelpad -f
```

如果觉得滚动太快或太慢，可在配置中调整 `scroll.sensitivity`（-2..+2）。守护进程以增量方式累积弦角增量，因此手指静止时不会滚动，滚动量与实际扫过的弧度成正比（参见 DECISIONS.md D-021-followup）。

## 已知问题 / 非目标

- **`WheelUnderCursor` 不可配置。** 在 Wayland 上，合成器将输入路由到焦点表面；没有用户态覆盖的办法。
- **仅测试过 Synaptics TM3562-3 系列。** 其他触摸板可能通过 `device_name_regex` 覆盖使用，但不作兼容性承诺。
- **Excel 方向键回退已移除。** 现代 Excel 原生支持水平滚轮事件；不再需要 Windows 的 hack。
- **无惯性/动能滚动。** 与 Windows WheelPad 行为一致；xf86 有但这里没有。

## 工作原理（一段话版）

守护进程在启动时独占物理触摸板（`EVIOCGRAB`，一直持有），并创建两个 `uinput` 虚拟设备供 libinput 接管：一个触摸板镜像（与物理触摸板能力相同）和一个滚轮。所有物理触摸事件原样转发到虚拟触摸板——因此光标、点按、点击和多指手势照常工作。当 6 状态 FSM（`Idle → Contact → Moving → Scrolling → Debounce`）判定手指在外圈画圆时，我们在该手势期间**抑制**转发（光标按预期冻结），并把弦方向角度积分进累加器。每次越过 ±π 就在虚拟滚轮上发出一个滚轮刻度。手指抬起时，我们转发抬起事件（剥离位置），让 libinput 看到一个干净的手势结束，而不产生合成光标跳变。

完整的算法细节和架构演变历史——参见 `DECISIONS.md`（D-022 是透传决策；D-008..D-021 是算法选择）以及源码旁的分析文档。

## 许可证

MIT。参见 [LICENSE](LICENSE)。

## 致谢

- Panasonic 提供了本项目所移植的原始 WheelPad 设计。
- X.Org `xf86-input-synaptics` 项目提供了逆向工程时对照的「绕中心点角度」参考实现。
- Peter Hutterer 的 [2015 libinput 讨论](https://gitlab.freedesktop.org/libinput/libinput/-/issues/) 解释了为什么这必须是一个守护进程而不是 libinput 补丁。
