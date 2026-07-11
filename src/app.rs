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

    /// Log every key event read and every report sent.
    #[arg(short, long)]
    verbose: bool,
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
        let link = wait_for_host(&peripheral).await?;
        println!("host connected: {}", link.peer());

        // A fresh link starts with nothing held. If the previous link died
        // mid-chord our state still says Command is down, and the Mac would
        // inherit a stuck modifier it never saw pressed.
        state.release_all();
        link.send(&state.wire_report()).await.ok();

        if let Err(e) = pump(&mut rx, &link, &mut state, args.verbose).await {
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

/// Get a link to a host, whichever way one turns up first.
///
/// Two things can happen and we cannot predict which: the Mac notices us and
/// opens the channels, or nobody comes and we have to knock. After a Pi reboot
/// the Mac generally does *not* reconnect on its own -- a real keyboard is
/// expected to announce itself -- so waiting passively would leave a keyboard
/// that only works if you go and click Connect. Race both.
async fn wait_for_host(peripheral: &HidPeripheral) -> Result<HidLink> {
    let known = peripheral.known_hosts().await.unwrap_or_default();
    if known.is_empty() {
        println!("no bonded host yet -- pair from the Mac");
        return peripheral.accept().await;
    }

    tokio::select! {
        incoming = peripheral.accept() => incoming,
        dialled = dial_any(peripheral, &known) => dialled,
    }
}

/// Knock on every bonded host until one lets us in.
///
/// Backs off up to 30s: a Mac that is asleep or out of range will refuse for a
/// long time, and hammering it drains the Pi and clogs the radio we also need
/// for WiFi.
async fn dial_any(peripheral: &HidPeripheral, hosts: &[bluer::Address]) -> Result<HidLink> {
    let mut delay = std::time::Duration::from_secs(2);
    loop {
        for &host in hosts {
            if let Ok(link) = peripheral.connect(host).await {
                return Ok(link);
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_secs(30));
    }
}

/// Forward key events to the host until the link breaks.
async fn pump(
    rx: &mut mpsc::Receiver<KeyEvent>,
    link: &HidLink,
    state: &mut KeyboardState,
    verbose: bool,
) -> Result<()> {
    while let Some(ev) = rx.recv().await {
        // `apply` returns false for events that do not change what the host can
        // see -- a repeated press of a held key, an unmapped key. Staying quiet
        // keeps the interrupt channel for things that matter.
        let changed = state.apply(ev.code, ev.pressed);
        if verbose {
            let action = if ev.pressed { "down" } else { "up  " };
            let report = state.report();
            let note = if changed { "" } else { "  (no change, not sent)" };
            eprintln!("evdev {:>3} {action} -> {report:02x?}{note}", ev.code);
        }
        if changed {
            link.send(&state.wire_report()).await?;
        }
    }
    anyhow::bail!("all keyboard devices went away");
}
