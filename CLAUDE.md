# CLAUDE.md -- rpi-hub working context

Context for continuing this project in a later session. The README is the
user-facing doc; this is the engineering record: what was decided, what was
*rejected and why*, the wire-level detail you would otherwise have to re-derive,
and the facts about the actual hardware.

Status as of 2026-07-11: **working end to end and running as a systemd service.**
Richard has typed real messages through it.

---

## 1. What this is

A Raspberry Pi 5 with a wired USB keyboard plugged into it presents *itself* to a
MacBook as a **Bluetooth HID keyboard**. Keystrokes on the wired keyboard appear
on the Mac. There is **no software on the Mac** -- it believes an ordinary
Bluetooth keyboard is connected.

Goal was a clean desk: one keyboard, no cable running to the laptop, Pi hidden.

```
USB keyboard --> [ Pi 5, Debian bookworm, BlueZ 5.66 ]     --> [ MacBook Air ]
                  evdev, exclusive grab (EVIOCGRAB)             an ordinary
                  evdev keycode -> USB HID usage code           Bluetooth keyboard
                  6KRO boot-protocol 8-byte report              nothing installed
                  L2CAP interrupt channel, PSM 19               no permission granted
                  SDP record advertises HID (UUID 0x1124)
                  Class-of-Device 0x002540 (keyboard)
```

---

## 2. Architecture decisions, and what was rejected

### 2.1 Bluetooth HID, NOT UDP + CGEventPost  (the big one)

The original request was a UDP client-server: Pi reads evdev, sends packets, a
Rust daemon on the Mac injects them via CoreGraphics `CGEventPost`. **Rejected
after measurement.** Do not resurrect it without re-reading this section.

| | UDP + CGEventPost | UDP + Karabiner DriverKit | **Bluetooth HID (chosen)** |
|---|---|---|---|
| Software on Mac | Rust daemon | Rust daemon + driver | **none** |
| macOS permission | Accessibility (TCC) | driver install | **none** |
| Runs as root on Mac | no | **yes** | **n/a** |
| Password fields (Secure Input) | **no** | yes | **yes** |
| Login window | **no** | yes | **yes** |
| FileVault pre-boot | no | no | no |
| Latency | WiFi: 21-60ms, spikes 223ms | same | ~5-20ms, steady |

Reasons, concretely:

* **`CGEventPost` is a second-class keyboard.** It cannot type into password
  fields with Secure Input enabled, nor at the login window. This is why
  Karabiner itself moved off event-tap injection.
* **The network path was genuinely bad.** Measured Pi -> Mac over the Pi's WiFi:
  `rtt min/avg/max/mdev = 21.5/31.1/73.5/14.5 ms`, and over Tailscale
  `6.7/59.8/222.8/62.3 ms`. Jitter that large means keys arrive in *clumps*. A
  real keyboard is 1-5ms.
* **Bluetooth HID deleted the entire Mac-side codebase** -- and with it the UDP
  reliability protocol (sequence numbers, redundancy, state resync), the crypto
  (keystrokes are passwords, so a UDP listener would have needed authenticating),
  and the evdev -> macOS-virtual-keycode table. All of that is *gone*, not
  postponed.

**Accepted limitation:** does NOT work at the **FileVault pre-boot unlock**
screen. macOS only connects Apple's own keyboards in that environment. *Neither
Bluetooth nor Karabiner can fix this -- only USB can.* Irrelevant here: the Mac is
a MacBook and its built-in keyboard is right there for that one screen. (An
earlier version of the plan wrongly claimed Bluetooth solved FileVault. It does
not.)

### 2.2 Classic BR/EDR HID, NOT BLE HOGP

BLE HID-over-GATT was considered and **rejected**: Apple filters the HID Service
(0x1812) at the OS level, and the one public BlueZ HOGP keyboard example reports
working to Android but **failing to macOS**. Classic BR/EDR HID over L2CAP is what
PiKVM ships in production, and it is what works here.

### 2.3 Boot protocol / 6KRO, NOT NKRO

The report map is the **standard boot-protocol keyboard descriptor**: 8 bytes, six
key slots. NKRO would need a custom bitmap descriptor, and hosts routinely
mis-parse nonstandard report maps. Six simultaneous keys is far more than typing
needs. Do not "upgrade" this.

### 2.4 evdev autorepeat is dropped on purpose

evdev emits `value == 2` for autorepeat. **We discard it.** A real HID keyboard
does not transmit repeats -- the *host* generates them from its own key-repeat
settings. Forwarding them doubles every held key. See `app.rs`, the `match value`.

---

## 3. THE BIG FINDING: macOS pairs, and does not ask for a passkey

This was the project's one genuine unknown and the reason a Phase-0 spike was run
before any Rust was written.

Prior art said this might be impossible:
* `EmuBTHID` reports flatly: *"Don't work with apple devices. Having a 'connection
  reset by peer' issue."*
* No public confirmation existed of a BlueZ-emulated BR/EDR HID keyboard pairing
  with a **modern** macOS.
* The feared trap: macOS's keyboard pairing flow ("type this 6-digit code and
  press Enter") would require **passkey-entry-as-keyboard** -- the Pi typing its
  own pairing code over a channel that is not yet established.

**Experiment result (2026-07-11): it just works.**
* A `NoInputNoOutput` BlueZ agent negotiates **Just Works** pairing.
* macOS **accepts it** even though the device advertises Class-of-Device =
  keyboard, which is self-contradictory in theory. It does not care.
* **No passkey was ever requested.** The passkey-entry-as-keyboard machinery was
  never needed and is NOT implemented.

**The real failure mode is a stale bond, not the protocol.** Symptom: the Mac
lists the device as paired and connects, but the Pi has no bond record at all
(`bluetoothctl info <mac>` returns nothing) and no agent callbacks fire. Cause:
removing the pairing on the Pi only removes the Pi's half; the Mac still holds a
link key from an older pairing and therefore **skips pairing entirely**. Fix:
**"Forget This Device" on the Mac too**, then re-pair. This is almost certainly
what the `connection reset by peer` reports in the wild actually are.

---

## 4. How the Bluetooth keyboard is actually mapped (wire-level)

### 4.1 The Bluetooth side

| thing | value | why |
|---|---|---|
| Profile UUID | `00001124-0000-1000-8000-00805f9b34fb` | HumanInterfaceDeviceServiceClass (0x1124) |
| Class-of-Device | `0x002540` | major = Peripheral, minor = Keyboard. Wrong CoD and macOS files us under the wrong accessory type and never asks for HID. |
| L2CAP PSM 17 (0x11) | control channel | host opens this first |
| L2CAP PSM 19 (0x13) | interrupt channel | **key reports go out here** |
| SDP record | see `bt.rs::sdp_record()` | carries the report map; registered via `org.bluez.Profile1` |

Two hard prerequisites on the Pi, both applied by `setup/install.sh`, both silent
and baffling failures if missed:

1. **`bluetoothd --noplugin=input`.** BlueZ normally loads its `input` plugin,
   which claims the HID **host** role -- it wants to talk *to* keyboards, and it
   binds PSMs 17/19 to do so. We need to *be* a keyboard. Without this, our
   listeners cannot bind.
2. **Root.** PSMs below 0x1000 are privileged. Non-root = bind fails.

We register the `Profile1` purely to publish the SDP record, but **own the two
L2CAP sockets ourselves** -- HID needs two channels and `Profile1` only hands back
one.

### 4.2 The HID report map (report descriptor)

Byte-exact, in `bt.rs::REPORT_MAP`. The host parses this to decide what our bytes
*mean*; one wrong byte and every keystroke silently vanishes.

```
05 01        Usage Page (Generic Desktop)
09 06        Usage (Keyboard)
a1 01        Collection (Application)
85 01          Report ID (1)
05 07          Usage Page (Keyboard/Keypad)
19 e0 29 e7    Usage Min/Max 224..231      <- the eight modifiers
15 00 25 01    Logical Min/Max 0..1
75 01 95 08    Report Size 1 x Count 8
81 02          Input (Data,Var,Abs)        <- byte 0: modifier bitmask
95 01 75 08
81 01          Input (Const)               <- byte 1: reserved
95 05 75 01    Report Count 5 x Size 1
05 08          Usage Page (LEDs)
19 01 29 05    Usage Min/Max 1..5
91 02          Output (Data,Var,Abs)       <- LED report (caps lock etc, host->us)
95 01 75 03
91 01          Output (Const)              <- LED padding to a byte
95 06 75 08    Report Count 6 x Size 8
15 00 25 65    Logical Min/Max 0..101
05 07          Usage Page (Keyboard/Keypad)
19 00 29 65    Usage Min/Max 0..101
81 00          Input (Data,Array)          <- bytes 2..8: six key slots
c0           End Collection
```

### 4.3 The input report

The **8-byte boot report** (`report.rs::KeyboardState::report`):

```
byte 0 : modifier bitmask
byte 1 : reserved, always 0
byte 2 : key slot 1  (USB HID usage code, 0 = empty)
byte 3 : key slot 2
byte 4 : key slot 3
byte 5 : key slot 4
byte 6 : key slot 5
byte 7 : key slot 6
```

Modifier bitmask, byte 0:

```
bit 0  0x01  Left Ctrl
bit 1  0x02  Left Shift
bit 2  0x04  Left Alt      -> macOS reads as Option
bit 3  0x08  Left GUI      -> macOS reads as Command
bit 4  0x10  Right Ctrl
bit 5  0x20  Right Shift
bit 6  0x40  Right Alt     -> macOS reads as Option
bit 7  0x80  Right GUI     -> macOS reads as Command
```

On the **L2CAP interrupt channel** it is framed as **10 bytes**
(`report.rs::wire_report`):

```
0xA1  HIDP header: DATA | Input
0x01  Report ID (matches the `85 01` in the report map)
...   the 8-byte report above
```

Example -- pressing `a` (usage 0x04) with no modifiers:

```
a1 01 00 00 04 00 00 00 00 00
      ^  ^  ^
      |  |  +-- key slot 1 = 0x04 ('a')
      |  +----- reserved
      +-------- modifiers = none
```

Release is the same frame with the slot cleared. If **more than six** non-modifier
keys are held, the entire slot array is filled with `0x01` (rollover error), per
spec -- and it must *drain* cleanly, which `report.rs` has a test for.

### 4.4 The modifier mapping -- Richard's explicit requirement

Modifiers map by **physical position**, not by keycap label:

```
PC keycaps:   [Ctrl] [Win]  [Alt]  [Space]
Mac expects:  [Ctrl] [Opt]  [Cmd]  [Space]
```

macOS reads HID **GUI** as Command and HID **Alt** as Option. So to put Command
under the key that physically sits where a Mac puts Command -- the one a PC board
labels **Alt** -- the **Alt and GUI bits are swapped**, both sides:

| evdev key | evdev code | HID bit emitted | macOS sees |
|---|---|---|---|
| `KEY_LEFTCTRL` | 29 | bit 0 `L_CTRL` | Control |
| `KEY_LEFTSHIFT` | 42 | bit 1 `L_SHIFT` | Shift |
| `KEY_LEFTMETA` (Win) | 125 | bit 2 `L_ALT` | **Option** |
| `KEY_LEFTALT` | 56 | bit 3 `L_GUI` | **Command** |
| `KEY_RIGHTCTRL` | 97 | bit 4 `R_CTRL` | Control |
| `KEY_RIGHTSHIFT` | 54 | bit 5 `R_SHIFT` | Shift |
| `KEY_RIGHTMETA` | 126 | bit 6 `R_ALT` | **Option** |
| `KEY_RIGHTALT` | 100 | bit 7 `R_GUI` | **Command** |

Ctrl and Shift are **never** swapped. `--literal-modifiers` honours the keycaps
instead (`ModifierLayout::Literal`).

### 4.5 evdev keycode -> USB HID usage code

**These are two unrelated numbering schemes** (and USB HID usages are a *third*
scheme distinct from both -- evdev codes are close-but-not-equal to HID usages, so
never assume). The table in `keymap.rs` is the inverse of the kernel's
`hid_keyboard[]` array in `drivers/hid/hid-input.c`.

Anchors worth remembering:

```
KEY_A   = 30  -> 0x04        letters run contiguously A..Z = 0x04..0x1D
KEY_1   = 2   -> 0x1E        digits run 1..9,0 = 0x1E..0x27  (note: 0 is LAST)
KEY_ENTER=28  -> 0x28
KEY_ESC = 1   -> 0x29
KEY_SPACE=57  -> 0x2C
KEY_F1  = 59  -> 0x3A        F1..F12 = 0x3A..0x45
KEY_102ND=86  -> 0x64        the ISO key next to left-shift
```

Deliberately **unmapped** (returns `Mapped::Ignored`, occupies no slot):
* **Media/consumer keys** (volume, brightness) -- these live on HID **usage page
  0x0C**, need a second report collection, and are **not implemented**. This is the
  most likely next feature.
* **Fn** -- swallowed by the keyboard's own firmware; never reaches evdev at all.

---

## 5. The hardware, concretely

| thing | value |
|---|---|
| Pi | Raspberry Pi 5 Model B, Debian bookworm, kernel 6.12, aarch64 |
| SSH | `ssh rpi` (user `richard`), `ssh rpir` (root) |
| Repo on Pi | `/home/richard/dev/rpi-hub` (owned by `richard`) |
| Pi Bluetooth MAC | `2C:CF:67:38:A1:16` |
| MacBook Bluetooth MAC | `A8:8F:D9:2F:A1:67` ("MacBook Air de Richard") |
| BlueZ | 5.66 |
| Keyboard | `SINO WEALTH Gaming Keyboard` |

**The keyboard is THREE device nodes.** Grabbing only the main one silently leaks
media/extra keys to the Pi's console. We grab the first two:

```
/dev/input/by-id/usb-SINO_WEALTH_Gaming_Keyboard-event-kbd    <- main, grabbed
/dev/input/by-id/usb-SINO_WEALTH_Gaming_Keyboard-event-if01   <- extra, grabbed
/dev/input/by-id/usb-SINO_WEALTH_Gaming_Keyboard-if01-event-mouse
```

**Always use the `/dev/input/by-id/` paths.** The `eventN` numbers are reassigned
on replug -- `-event-kbd` moved from `event12` to `event5` *during a single
development session*.

### The Pi was previously a Bluetooth audio receiver

Before this project the Pi was `rpi-audio`, CoD `0x200414` (Audio/Loudspeaker),
with a `BSBA-02` speaker paired. **rpi-hub takes that role over** -- one adapter,
one Class-of-Device. `setup/uninstall.sh` restores it; the original `main.conf` is
backed up at `/root/rpi-hub-backup/main.conf.orig`.

### WiFi/Bluetooth share one radio

`eth0` is DOWN; the Pi's only uplink is 2.4GHz WiFi, on the **same Broadcom combo
chip** as Bluetooth. Keystrokes compete with the Pi's own SSH/network traffic. If
typing ever feels rubbery, the fix is **Ethernet or 5GHz WiFi -- not code.**

---

## 6. Code layout

| file | what | Linux-only? |
|---|---|---|
| `src/keymap.rs` | evdev keycode -> HID usage; the Alt/GUI swap | no |
| `src/report.rs` | 6KRO state machine, boot report, wire framing | no |
| `src/input.rs` | evdev capture, exclusive grab, ungrab on Drop | **yes** |
| `src/bt.rs` | SDP record, report map, L2CAP accept + dial-out | **yes** |
| `src/app.rs` | the bridge loop, CLI | **yes** |
| `setup/install.sh` | BlueZ config + build + systemd | |
| `setup/uninstall.sh` | restore the Pi's previous Bluetooth role | |

`keymap` and `report` are **pure logic with no Linux dependency**, gated so that
`cargo test` runs them on macOS during development (21 tests). `input`/`bt`/`app`
are `#[cfg(target_os = "linux")]`. Keep it that way -- it is why the hard parts are
testable without hardware.

### Build / deploy loop

Richard's instruction: **transfer via git**. Push on the Mac, pull on the Pi.

```sh
# on the Mac
cargo test && git push

# on the Pi -- NOTE: pull/build as `richard`, NOT root.
# git refuses to operate on a repo it does not own ("dubious ownership").
ssh rpi 'cd ~/dev/rpi-hub && git pull && cargo build --release'
ssh rpir 'systemctl restart rpi-hub'
journalctl -u rpi-hub -f
```

---

## 7. Landmines

* **The grab is a footgun.** While the service runs, the keyboard belongs to the
  Mac and is **dead on the Pi**. If the process dies without ungrabbing, SSH is the
  only way back in. `GrabbedDevice` ungrabs on `Drop`; the unit is `Restart=always`.
  (This bit us for real: killing the process to rebuild yanked the keyboard out
  from under Richard mid-sentence.)
* **`git` as root on the Pi fails** with "dubious ownership" -- and in a pipeline
  it can fail *silently*. Build as `richard`.
* **`StartLimitIntervalSec` belongs in `[Unit]`, not `[Service]`.** systemd ignores
  it in `[Service]` with only a warning.
* **A stale bond on the Mac** makes pairing appear to succeed while the Pi sees
  nothing. Forget the device on the Mac side too.
* **Don't forward evdev autorepeat.** See 2.4.

---

## 8. Where to go next

Not started, roughly in order of value:

1. **Mouse.** The Pi already has the keyboard's mouse node
   (`-if01-event-mouse`), and the "hub" framing wants it. Needs a second HID
   report collection + descriptor, relative-motion reports, and the SDP record
   extended. PiKVM's warning applies: many hosts mis-parse nonstandard
   descriptors, so keep the mouse to **relative mode** and consider dropping
   horizontal scroll.
2. **Media/consumer keys** (volume, brightness). HID usage page 0x0C, second
   report collection. Currently `Mapped::Ignored`.
3. **Sniff-mode latency.** BR/EDR HID negotiates sniff mode to save power; the
   *first keypress after an idle gap* can be delayed up to ~100ms. Steady-state
   typing is fine. If Richard notices a lag on the first key after a pause, this
   is the cause -- request shorter sniff intervals from the Pi.
4. **LED / caps-lock output reports.** The descriptor already declares the LED
   output report, but we never read from the control channel, so the Mac's caps
   lock state never lights the keyboard's LED.
5. **Multi-host.** The Pi stops being discoverable once connected. Switching
   between Mac/iPad would need bthidhub-style multiplexing.

## 9. Unverified

* **Alt+C / Alt+V actually producing Cmd+C / Cmd+V on the Mac** was never
  confirmed by a human, though `report.rs` asserts the byte-level mapping
  (`alt_plus_c_is_delivered_to_macos_as_command_c`). Confirm before trusting.
* Behaviour at the **login window** and in **Secure Input password fields** is
  expected to work (it is a real HID keyboard) but was **not tested**.
* Reconnect after a **Mac sleep/wake** cycle -- the dial-out path exists and works
  after a *service* restart, but a full Mac sleep was not exercised.
