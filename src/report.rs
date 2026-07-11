//! The HID boot-protocol keyboard report and the state machine behind it.
//!
//! A boot keyboard report is 8 bytes:
//!
//! ```text
//!   [0] modifier bitmask
//!   [1] reserved (always 0)
//!   [2..8] up to six simultaneously-held key usage codes
//! ```
//!
//! Six slots is the whole budget -- hence "6KRO". Real keyboards behave this way
//! and it is more than enough for typing, so we deliberately do not attempt an
//! NKRO descriptor: hosts routinely mis-parse nonstandard report maps, and macOS
//! is not a host worth gambling with.

use crate::keymap::{self, Mapped, ModifierLayout};

/// Number of key slots in a boot-protocol report.
pub const KEY_SLOTS: usize = 6;

/// HID usage code meaning "too many keys held at once".
const ROLLOVER_ERROR: u8 = 0x01;

/// HIDP header: DATA | Input.
const HIDP_DATA_INPUT: u8 = 0xA1;

/// Report ID, matching the `85 01` in our report map.
const REPORT_ID: u8 = 0x01;

/// Tracks which keys are currently held, and renders boot-protocol reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyboardState {
    modifiers: u8,
    keys: [u8; KEY_SLOTS],
    layout: ModifierLayout,
}

impl KeyboardState {
    pub fn new(layout: ModifierLayout) -> Self {
        Self { modifiers: 0, keys: [0; KEY_SLOTS], layout }
    }

    /// Apply an evdev key event.
    ///
    /// `pressed` is the evdev value: `true` for press, `false` for release.
    /// Autorepeat (evdev value 2) must NOT be routed here -- a real HID keyboard
    /// does not transmit repeats, the host synthesises them. Forwarding repeats
    /// produces doubled characters.
    ///
    /// Returns `true` if the visible state changed and a new report should be
    /// sent. Redundant events (a press for a key already held) return `false`,
    /// so we stay silent rather than spamming the interrupt channel.
    pub fn apply(&mut self, evdev_code: u16, pressed: bool) -> bool {
        match keymap::map(evdev_code, self.layout) {
            Mapped::Modifier(bit) => {
                let before = self.modifiers;
                if pressed {
                    self.modifiers |= bit;
                } else {
                    self.modifiers &= !bit;
                }
                self.modifiers != before
            }
            Mapped::Key(usage) => {
                if pressed {
                    self.press(usage)
                } else {
                    self.release(usage)
                }
            }
            Mapped::Ignored => false,
        }
    }

    fn press(&mut self, usage: u8) -> bool {
        if self.keys.contains(&usage) {
            return false; // already held
        }
        if let Some(slot) = self.keys.iter_mut().find(|s| **s == 0) {
            *slot = usage;
            true
        } else {
            // More than six keys down. Per the HID spec the entire array is
            // filled with the rollover-error code, not just the overflowing key.
            self.keys = [ROLLOVER_ERROR; KEY_SLOTS];
            true
        }
    }

    fn release(&mut self, usage: u8) -> bool {
        // If we were in rollover, the array holds error codes rather than real
        // usages, so we cannot know which slot this key occupied. Clear it and
        // let the still-held keys re-register on their next event.
        if self.keys[0] == ROLLOVER_ERROR {
            self.keys = [0; KEY_SLOTS];
            return true;
        }
        match self.keys.iter_mut().find(|s| **s == usage) {
            Some(slot) => {
                *slot = 0;
                true
            }
            None => false,
        }
    }

    /// Forget every held key and modifier.
    ///
    /// Sent before tearing down a connection: without it the Mac is left holding
    /// whatever was down at the moment the link dropped, which in practice means
    /// a Command key stuck forever.
    pub fn release_all(&mut self) -> bool {
        let changed = self.modifiers != 0 || self.keys.iter().any(|&k| k != 0);
        self.modifiers = 0;
        self.keys = [0; KEY_SLOTS];
        changed
    }

    /// The bare 8-byte boot report.
    pub fn report(&self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0] = self.modifiers;
        out[1] = 0; // reserved
        out[2..].copy_from_slice(&self.keys);
        out
    }

    /// The report framed for the L2CAP interrupt channel: HIDP header, report
    /// ID, then the report itself.
    pub fn wire_report(&self) -> [u8; 10] {
        let r = self.report();
        let mut out = [0u8; 10];
        out[0] = HIDP_DATA_INPUT;
        out[1] = REPORT_ID;
        out[2..].copy_from_slice(&r);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::evdev_codes::*;
    use crate::keymap::modbit;

    fn kb() -> KeyboardState {
        KeyboardState::new(ModifierLayout::MacPositional)
    }

    #[test]
    fn a_single_keypress_fills_the_first_slot() {
        let mut k = kb();
        assert!(k.apply(KEY_A, true));
        assert_eq!(k.report(), [0, 0, 0x04, 0, 0, 0, 0, 0]);
        assert!(k.apply(KEY_A, false));
        assert_eq!(k.report(), [0; 8]);
    }

    #[test]
    fn the_wire_frame_carries_the_hidp_header_and_report_id() {
        let mut k = kb();
        k.apply(KEY_A, true);
        assert_eq!(k.wire_report(), [0xA1, 0x01, 0, 0, 0x04, 0, 0, 0, 0, 0]);
    }

    /// The point of the whole modifier layout: physical Alt produces Command.
    #[test]
    fn alt_plus_c_is_delivered_to_macos_as_command_c() {
        let mut k = kb();
        k.apply(KEY_LEFTALT, true);
        k.apply(KEY_C, true);
        assert_eq!(k.report(), [modbit::L_GUI, 0, 0x06, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn releasing_a_middle_key_does_not_disturb_its_neighbours() {
        let mut k = kb();
        k.apply(KEY_A, true);
        k.apply(KEY_B, true);
        k.apply(KEY_C, true);
        k.apply(KEY_B, false);
        // The hole is left in place; keys do not shuffle down.
        assert_eq!(k.report(), [0, 0, 0x04, 0, 0x06, 0, 0, 0]);
        // ...and the freed slot is reused by the next press.
        k.apply(KEY_D, true);
        assert_eq!(k.report(), [0, 0, 0x04, 0x07, 0x06, 0, 0, 0]);
    }

    #[test]
    fn repeated_press_of_a_held_key_emits_nothing() {
        let mut k = kb();
        assert!(k.apply(KEY_A, true));
        assert!(!k.apply(KEY_A, true), "a redundant press must not resend");
    }

    #[test]
    fn releasing_a_key_that_was_never_down_emits_nothing() {
        let mut k = kb();
        assert!(!k.apply(KEY_A, false));
    }

    #[test]
    fn a_seventh_key_triggers_rollover_error() {
        let mut k = kb();
        for c in [KEY_A, KEY_B, KEY_C, KEY_D, KEY_E, KEY_F] {
            k.apply(c, true);
        }
        assert_eq!(k.report(), [0, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]);
        k.apply(KEY_G, true);
        assert_eq!(k.report(), [0, 0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01]);
    }

    #[test]
    fn rollover_clears_rather_than_wedging() {
        // Coming out of rollover must not leave phantom 0x01s held forever.
        let mut k = kb();
        for c in [KEY_A, KEY_B, KEY_C, KEY_D, KEY_E, KEY_F, KEY_G] {
            k.apply(c, true);
        }
        k.apply(KEY_G, false);
        assert_eq!(k.report(), [0; 8], "rollover must drain, not stick");
    }

    #[test]
    fn modifiers_survive_key_traffic() {
        let mut k = kb();
        k.apply(KEY_LEFTSHIFT, true);
        k.apply(KEY_A, true);
        k.apply(KEY_A, false);
        assert_eq!(k.report()[0], modbit::L_SHIFT, "shift must still be held");
    }

    #[test]
    fn release_all_clears_keys_and_modifiers() {
        let mut k = kb();
        k.apply(KEY_LEFTALT, true);
        k.apply(KEY_A, true);
        assert!(k.release_all());
        assert_eq!(k.report(), [0; 8]);
        // Idempotent: nothing left to clear means nothing to send.
        assert!(!k.release_all());
    }

    /// The failure this guards against is the nastiest one in the system: the
    /// link drops mid-chord and the Mac is left holding Command forever.
    #[test]
    fn release_all_rescues_a_stuck_modifier() {
        let mut k = kb();
        k.apply(KEY_LEFTALT, true); // -> Command, held
        assert_ne!(k.report()[0], 0);
        k.release_all();
        assert_eq!(k.report()[0], 0, "Command must not survive a disconnect");
    }

    #[test]
    fn ignored_keys_never_occupy_a_slot() {
        let mut k = kb();
        assert!(!k.apply(0xFFFF, true));
        assert_eq!(k.report(), [0; 8]);
    }
}
