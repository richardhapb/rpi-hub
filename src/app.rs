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

    /// Bluetooth address of a host to dial on reconnect, e.g. A8:8F:D9:2F:A1:67.
    ///
    /// Only these addresses are ever dialled. Pass it more than once for more
    /// than one host; with none given we never knock, and only wait for a host
    /// to open the channels.
    ///
    /// It is deliberately not "every paired device": a Pi accumulates pairings
    /// for speakers, phones and projectors, and at least an iPhone will happily
    /// accept both L2CAP channels -- taking the keyboard hostage on a link the
    /// Mac can never win.
    #[arg(long = "host")]
    hosts: Vec<bluer::Address>,

    /// Log every key event read and every report sent.
    #[arg(short, long)]
    verbose: bool,
}

/// One key transition, already normalised out of evdev's event zoo.
struct KeyEvent {
    code: u16,
    pressed: bool,
}

/// Why the pump stopped. The two cases look alike and are not alike at all.
enum PumpEnd {
    /// The host's L2CAP link broke: it slept, wandered out of range, or was
    /// disconnected. Recoverable -- wait for it to come back.
    HostGone(anyhow::Error),

    /// Every keyboard node stopped reading, which in practice means the keyboard
    /// was unplugged or the USB port reset it. Fatal: the evdev file descriptors
    /// we hold are dead, and nothing in this process reopens them. The device
    /// nodes that come back on replug are *new* nodes (and often new eventN
    /// numbers), so the only honest move is to exit and be restarted onto them.
    InputGone,
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

    // An untrusted device makes BlueZ ask an agent for authorisation on every
    // reconnect, and we run headless with nobody to ask. This pass only catches
    // hosts that are *already* bonded; one that pairs later is trusted on connect
    // (see the loop below), which is the case that actually bit us.
    for &host in &args.hosts {
        peripheral.trust(host).await?;
        println!("will dial {host} on reconnect");
    }
    if args.hosts.is_empty() {
        println!("no --host pinned -- waiting for the host to connect to us");
    }

    let mut state = KeyboardState::new(layout);

    loop {
        let link = wait_for_host(&peripheral, &args.hosts).await?;
        println!("host connected: {}", link.peer());

        // Trust it now, not at startup. A host that pairs *after* we boot -- which
        // is every first pairing, and every re-pair after a bond is cleared -- is
        // not paired when the loop above runs, so that pass skips it and it stays
        // untrusted forever. BlueZ then asks an agent to authorise each reconnect,
        // finds none (we are headless), and the link silently stops coming back.
        // By the time we have a link the host is necessarily bonded, so this is the
        // one place the trust can never be premature.
        if let Err(e) = peripheral.trust(link.peer()).await {
            eprintln!("warning: could not trust {}: {e}", link.peer());
        }

        // A fresh link starts with nothing held. If the previous link died
        // mid-chord our state still says Command is down, and the Mac would
        // inherit a stuck modifier it never saw pressed.
        state.release_all();
        link.send(&state.wire_report()).await.ok();

        let end = pump(&mut rx, &link, &mut state, args.verbose).await;

        // Tell the host to let go of everything before the channel disappears.
        // Best-effort: if the link is already gone this send fails, and that is
        // fine -- a host that lost the link drops held keys itself.
        if state.release_all() {
            link.send(&state.wire_report()).await.ok();
        }

        match end {
            // Losing the keyboard is not "the host went away", and looping back
            // to wait for a host would spin forever against a dead channel:
            // every future pump returns instantly, and we would hammer the radio
            // reconnecting to a host we have nothing to say to. Exit instead --
            // systemd restarts us, and the restart reopens the device nodes.
            PumpEnd::InputGone => {
                anyhow::bail!("all keyboard devices went away (unplugged?) -- exiting to be restarted onto the new device nodes")
            }
            PumpEnd::HostGone(e) => {
                eprintln!("link to {} lost: {e}", link.peer());
                println!("host disconnected; waiting for reconnect");
            }
        }
    }
}

/// Get a link to a host, whichever way one turns up first.
///
/// Two things can happen and we cannot predict which: the Mac notices us and
/// opens the channels, or nobody comes and we have to knock. After a Pi reboot
/// the Mac generally does *not* reconnect on its own -- a real keyboard is
/// expected to announce itself -- so waiting passively would leave a keyboard
/// that only works if you go and click Connect. Race both.
async fn wait_for_host(peripheral: &HidPeripheral, hosts: &[bluer::Address]) -> Result<HidLink> {
    if hosts.is_empty() {
        return peripheral.accept().await;
    }

    tokio::select! {
        incoming = peripheral.accept() => incoming,
        dialled = dial_any(peripheral, hosts) => dialled,
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

/// Forward key events to the host until the link breaks or the keyboard does.
async fn pump(
    rx: &mut mpsc::Receiver<KeyEvent>,
    link: &HidLink,
    state: &mut KeyboardState,
    verbose: bool,
) -> PumpEnd {
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
            if let Err(e) = link.send(&state.wire_report()).await {
                return PumpEnd::HostGone(e);
            }
        }
    }
    // The channel closes only when every reader task has exited, i.e. every
    // keyboard node stopped reading. That is the keyboard, not the host.
    PumpEnd::InputGone
}
