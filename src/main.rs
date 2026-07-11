//! rpi-hub: a Raspberry Pi that turns a wired USB keyboard into a Bluetooth one.
//!
//! The Pi grabs the keyboard exclusively over evdev and re-presents itself to the
//! Mac as a Bluetooth HID keyboard. The Mac needs no software at all -- it just
//! sees a keyboard, which is why this works in password fields and at the login
//! window where synthetic-event injection does not.
//!
//! Only the Bluetooth and evdev halves are Linux-only. The keymap and the report
//! state machine are pure logic and their tests run anywhere, including on the
//! Mac this is developed from.

mod keymap;
mod report;

#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod bt;
#[cfg(target_os = "linux")]
mod input;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "rpi-hub runs on the Pi. This build exists so `cargo test` can exercise \
         the keymap and report logic on a non-Linux host."
    );
    std::process::exit(1);
}
