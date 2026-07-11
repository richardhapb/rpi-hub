//! The bridge itself: read the wired keyboard, speak HID to the Mac.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use evdev::{EventSummary, KeyCode};
use tokio::sync::mpsc;

use crate::bt::{HidLink, HidPeripheral};
use crate::input::{self, GrabbedDevice};
use crate::keymap::ModifierLayout;
use crate::report::KeyboardState;

#[derive(Parser, Debug)]
#[command(name = "rpi-hub", about = "Bridge a wired USB keyboard to a Mac over Bluetooth HID")]
struct Args {
    /// Keyboard device nodes to capture. Prefer the stable /dev/input/by-id/
    /// symlinks: the eventN numbers are reassigned when a device is replugged.
    ///
    /// Pass this more than once. A gaming keyboard typically presents several
    /// nodes, and keys on an ungrabbed node leak through to the Pi's console.
    #[arg(short, long = "device")]
    devices: Vec<PathBuf>,

    /// List keyboard-like input devices and exit.
    #[arg(long)]
    list: bool,

    /// Emit the modifiers the keycaps actually say, instead of preserving
    /// physical position (which puts Command under the PC Alt key).
    #[arg(long)]
    literal_modifiers: bool,

    /// Name shown on the Mac's Bluetooth list.
    #[arg(long, default_value = "rpi-hub Keyboard")]
    alias: String,
}

/// One key transition, already normalised out of evdev's event zoo.
struct KeyEvent {
    code: u16,
    pressed: bool,
}

pub async fn run() -> Result<()> {
    let args = Args::parse();

    if args.list {
        for (path, name) in input::discover() {
            println!("{}  {}", path.display(), name);
        }
        return Ok(());
    }

    if args.devices.is_empty() {
        anyhow::bail!("no --device given (try --list to see what is available)");
    }

    let layout = if args.literal_modifiers {
        ModifierLayout::Literal
    } else {
        ModifierLayout::MacPositional
    };

    // Grab every requested node before touching Bluetooth, so that a typo in a
    // device path fails fast instead of after the Mac has connected.
    let (tx, mut rx) = mpsc::channel::<KeyEvent>(256);
    for path in &args.devices {
        let device = GrabbedDevice::open(path)?;
        println!("grabbed {} ({})", device.path().display(), device.name());
        let tx = tx.clone();
        let mut stream = device.into_stream()?;
        tokio::spawn(async move {
            loop {
                let event = match stream.next_event().await {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("input stream ended: {e}");
                        return;
                    }
                };
                // EV_SYN and EV_MSC frames are interleaved with real keys; only
                // EV_KEY matters. evdev value 2 is autorepeat, which we drop --
                // a real HID keyboard does not transmit repeats, the host
                // generates them. Forwarding them doubles every held key.
                if let EventSummary::Key(_, KeyCode(code), value) = event.destructure() {
                    let pressed = match value {
                        0 => false,
                        1 => true,
                        _ => continue,
                    };
                    if tx.send(KeyEvent { code, pressed }).await.is_err() {
                        return;
                    }
                }
            }
        });
    }
    drop(tx);

    let peripheral = HidPeripheral::new(&args.alias)
        .await
        .context("bringing up the Bluetooth HID peripheral")?;
    println!("advertising as '{}' -- pair from the Mac", args.alias);

    let mut state = KeyboardState::new(layout);

    loop {
        let link = peripheral.accept().await?;
        println!("host connected: {}", link.peer());

        // A fresh link starts with nothing held. If the previous link died
        // mid-chord our state still says Command is down, and the Mac would
        // inherit a stuck modifier it never saw pressed.
        state.release_all();
        link.send(&state.wire_report()).await.ok();

        if let Err(e) = pump(&mut rx, &link, &mut state).await {
            eprintln!("link to {} lost: {e}", link.peer());
        }

        // Tell the host to let go of everything before the channel disappears.
        // Best-effort: if the link is already gone this send fails, and that is
        // fine -- a host that lost the link drops held keys itself.
        if state.release_all() {
            link.send(&state.wire_report()).await.ok();
        }
        println!("host disconnected; waiting for reconnect");
    }
}

/// Forward key events to the host until the link breaks.
async fn pump(
    rx: &mut mpsc::Receiver<KeyEvent>,
    link: &HidLink,
    state: &mut KeyboardState,
) -> Result<()> {
    while let Some(ev) = rx.recv().await {
        // `apply` returns false for events that do not change what the host can
        // see -- a repeated press of a held key, an unmapped key. Staying quiet
        // keeps the interrupt channel for things that matter.
        if state.apply(ev.code, ev.pressed) {
            link.send(&state.wire_report()).await?;
        }
    }
    anyhow::bail!("all keyboard devices went away");
}
