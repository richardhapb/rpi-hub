//! Exclusive capture of a wired keyboard via evdev.
//!
//! Two things here are load-bearing and easy to get wrong:
//!
//! 1. **Grabbing is a footgun.** `EVIOCGRAB` makes us the only reader of the
//!    device. If this process dies while holding a grab, the physical keyboard
//!    goes dead on the Pi itself, and SSH becomes the only way back in. Hence
//!    [`GrabbedDevice`] ungrabs on `Drop`.
//!
//! 2. **A keyboard is usually more than one device node.** Gaming keyboards
//!    split media keys and extra keys onto a second HID interface. Grabbing only
//!    the main node silently leaks those keys to the Pi's console.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use evdev::{Device, EventStream};

/// A keyboard node we hold an exclusive grab on.
///
/// The grab is released when this is dropped, including on panic (the runtime
/// runs destructors while unwinding), so the Pi's own keyboard comes back.
pub struct GrabbedDevice {
    path: PathBuf,
    device: Option<Device>,
}

impl GrabbedDevice {
    /// Open `path` and take an exclusive grab on it.
    pub fn open(path: &Path) -> Result<Self> {
        let mut device = Device::open(path)
            .with_context(|| format!("opening {}", path.display()))?;

        device
            .grab()
            .with_context(|| format!("grabbing {} (is another process holding it?)", path.display()))?;

        Ok(Self { path: path.to_path_buf(), device: Some(device) })
    }

    pub fn name(&self) -> String {
        self.device
            .as_ref()
            .and_then(|d| d.name())
            .unwrap_or("<unnamed>")
            .to_string()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consume this handle and turn it into an async event stream.
    ///
    /// The grab outlives this call: the stream owns the device, and dropping the
    /// stream drops the device, which ungrabs it in the kernel.
    pub fn into_stream(mut self) -> Result<EventStream> {
        let device = self.device.take().expect("device taken twice");
        device.into_event_stream().context("creating event stream")
    }
}

impl Drop for GrabbedDevice {
    fn drop(&mut self) {
        if let Some(device) = self.device.as_mut() {
            // Best-effort. If this fails the keyboard stays captured, which is
            // exactly the situation worth shouting about.
            if let Err(e) = device.ungrab() {
                eprintln!("WARNING: failed to ungrab {}: {e}", self.path.display());
            }
        }
    }
}

/// Does this device look like something that sends keystrokes?
///
/// `EV_KEY` alone is not enough -- mice, power buttons and lid switches all
/// report EV_KEY. Requiring a few keys that only a real keyboard has keeps those
/// out.
pub fn is_keyboard(device: &Device) -> bool {
    use evdev::KeyCode;

    let Some(keys) = device.supported_keys() else {
        return false;
    };
    keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_Z) && keys.contains(KeyCode::KEY_ENTER)
}

/// Find every keyboard-ish node, for `--list` and for autodetection.
pub fn discover() -> Vec<(PathBuf, String)> {
    evdev::enumerate()
        .filter(|(_, d)| is_keyboard(d))
        .map(|(p, d)| (p, d.name().unwrap_or("<unnamed>").to_string()))
        .collect()
}
