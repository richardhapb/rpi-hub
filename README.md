# rpi-hub

Plug a wired USB keyboard into a Raspberry Pi, and type on your Mac.

The Pi grabs the keyboard exclusively over evdev and re-presents itself to the
Mac as a **Bluetooth HID keyboard**. The Mac needs no software at all -- it just
sees a keyboard. The desk keeps one keyboard and no cable to the laptop; the Pi
lives out of sight.

```
USB keyboard --> [ Pi ]                                 --> [ Mac ]
                  evdev, exclusive grab                      an ordinary
                  evdev keycode -> USB HID usage code        Bluetooth keyboard.
                  8-byte boot-protocol report                Nothing installed.
                  L2CAP interrupt channel (PSM 19)
```

## Why Bluetooth HID and not a network protocol

The obvious design is a UDP client/server: Pi sends keystrokes, a daemon on the
Mac injects them with CoreGraphics `CGEventPost`. It was measured and rejected.

* **`CGEventPost` is a second-class keyboard.** It cannot type into password
  fields with Secure Input enabled, nor at the login window. Fixing that means
  installing the Karabiner DriverKit driver *and* running the Mac daemon as root.
* **The network path was bad.** Over the Pi's WiFi, latency to the Mac measured
  21-60ms average, jitter 13-62ms, spikes to 223ms. Keys arrive in clumps.
* **Bluetooth HID removes both problems and the entire Mac-side codebase.**
  Secure Input and the login window work because it is a real HID device -- there
  is nothing to permit, and nothing to install.

**Known limitation:** this does not work at the FileVault *pre-boot* unlock
screen. macOS only connects Apple's own keyboards there. Neither Bluetooth nor
Karabiner can change that -- only USB can. Use the MacBook's built-in keyboard
for that one screen.

## Modifiers

Modifiers are mapped by **physical position**, so your thumb finds Command where
a Mac keyboard puts it:

```
PC keycaps:   [Ctrl] [Win]  [Alt]  [Space]
Mac expects:  [Ctrl] [Opt]  [Cmd]  [Space]
```

macOS reads the HID *GUI* bit as Command and the *Alt* bit as Option, so the Alt
and GUI bits are swapped: the key labelled **Alt** on a PC board emits Command.
Pass `--literal-modifiers` to honour the keycaps instead.

## Install (on the Pi)

```sh
git clone https://github.com/richardhapb/rpi-hub.git ~/dev/rpi-hub
sudo ~/dev/rpi-hub/setup/install.sh
```

Then pair **"rpi-hub Keyboard"** from the Mac and type. It reconnects on its own
after a reboot or a sleep.

`setup/uninstall.sh` puts the Pi's Bluetooth back the way it was.

### The two non-obvious BlueZ requirements

Both are handled by `install.sh`, and both are silent, confusing failures if
missed:

1. **`bluetoothd --noplugin=input`.** BlueZ normally loads its `input` plugin,
   which claims the HID *host* role -- it wants to talk *to* keyboards, and takes
   L2CAP PSMs 17 and 19 to do it. We need to *be* a keyboard, so the plugin must
   go or our listeners cannot bind.
2. **Class-of-Device `0x002540`.** Left as anything else, macOS files the Pi
   under the wrong kind of accessory and never asks it for HID.

The Pi must also run this as **root**: L2CAP PSMs 17 and 19 are below 0x1000 and
therefore privileged.

## Layout

| file | what |
|---|---|
| `src/keymap.rs` | evdev keycode -> USB HID usage; the Alt/GUI swap |
| `src/report.rs` | 6-key-rollover state machine, boot-protocol reports |
| `src/input.rs` | evdev capture, exclusive grab, ungrab on drop |
| `src/bt.rs` | SDP record, L2CAP channels, accept + dial-out |
| `src/app.rs` | the bridge loop |

`keymap` and `report` are pure logic with no Linux dependency, so `cargo test`
runs them on any host -- including the Mac you develop on. `input` and `bt` are
gated to Linux.

## Gotchas worth knowing

* **The grab is a footgun.** While running, the keyboard belongs to the Mac and
  is dead on the Pi. If the process dies without ungrabbing, SSH is the way back
  in. `GrabbedDevice` ungrabs on `Drop`, and the service restarts always.
* **A keyboard is often several device nodes.** This one presents `-event-kbd`
  *and* `-event-if01`; grabbing only the first leaks media keys to the Pi's
  console. Always open the `/dev/input/by-id/` paths -- the `eventN` numbers are
  reassigned on replug (this keyboard moved from `event12` to `event5` during
  development).
* **evdev autorepeat is dropped on purpose.** A real HID keyboard does not
  transmit repeats; the host generates them. Forwarding evdev's `value == 2`
  doubles every held key.
* **WiFi and Bluetooth share one radio on the Pi.** If the Pi's only uplink is
  2.4GHz WiFi, your keystrokes compete with your own SSH traffic. Put the Pi's
  WiFi on 5GHz, or give it Ethernet.
