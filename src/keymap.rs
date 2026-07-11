//! Translation from Linux evdev key codes to USB HID usage codes.
//!
//! These are two unrelated numbering schemes. The table below is the inverse of
//! the kernel's `hid_keyboard[]` array in `drivers/hid/hid-input.c`.
//!
//! Modifiers are not usage codes at all -- they are bits in byte 0 of the HID
//! report -- so they come back as [`Mapped::Modifier`] instead.

/// Bits in the HID report's modifier byte.
pub mod modbit {
    pub const L_CTRL: u8 = 1 << 0;
    pub const L_SHIFT: u8 = 1 << 1;
    pub const L_ALT: u8 = 1 << 2;
    pub const L_GUI: u8 = 1 << 3;
    pub const R_CTRL: u8 = 1 << 4;
    pub const R_SHIFT: u8 = 1 << 5;
    pub const R_ALT: u8 = 1 << 6;
    pub const R_GUI: u8 = 1 << 7;
}

/// What an evdev code turns into on the HID side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapped {
    /// A usage code occupying one of the 6 key slots.
    Key(u8),
    /// A bit in the modifier byte.
    Modifier(u8),
    /// No HID equivalent (or deliberately unmapped).
    Ignored,
}

/// How the physical modifier keys are wired to HID modifier bits.
///
/// A PC keyboard and a Mac keyboard disagree about what lives left of the
/// spacebar:
///
/// ```text
/// PC:   [Ctrl] [Win] [Alt] [Space]
/// Mac:  [Ctrl] [Opt] [Cmd] [Space]
/// ```
///
/// macOS reads the HID GUI bit as Command and the Alt bit as Option. So to put
/// Command under the key that is physically where a Mac puts Command -- the one
/// labelled Alt on a PC board -- the Alt and GUI bits must be swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModifierLayout {
    /// Preserve *physical position*: PC-Alt emits GUI (Command), PC-Win emits
    /// Alt (Option). Your thumb finds Command where a Mac keyboard has it.
    #[default]
    MacPositional,
    /// Emit exactly what the keycaps say. PC-Alt emits Alt (Option), PC-Win
    /// emits GUI (Command).
    Literal,
}

impl ModifierLayout {
    /// Map an evdev modifier code to its HID modifier bit under this layout.
    fn bit(self, code: u16) -> Option<u8> {
        use evdev_codes::*;
        let literal = match code {
            KEY_LEFTCTRL => modbit::L_CTRL,
            KEY_LEFTSHIFT => modbit::L_SHIFT,
            KEY_LEFTALT => modbit::L_ALT,
            KEY_LEFTMETA => modbit::L_GUI,
            KEY_RIGHTCTRL => modbit::R_CTRL,
            KEY_RIGHTSHIFT => modbit::R_SHIFT,
            KEY_RIGHTALT => modbit::R_ALT,
            KEY_RIGHTMETA => modbit::R_GUI,
            _ => return None,
        };
        Some(match self {
            ModifierLayout::Literal => literal,
            ModifierLayout::MacPositional => swap_alt_gui(literal),
        })
    }
}

/// Exchange the Alt and GUI bits on both sides, leaving Ctrl/Shift alone.
fn swap_alt_gui(bits: u8) -> u8 {
    let mut out = bits & !(modbit::L_ALT | modbit::L_GUI | modbit::R_ALT | modbit::R_GUI);
    if bits & modbit::L_ALT != 0 {
        out |= modbit::L_GUI;
    }
    if bits & modbit::L_GUI != 0 {
        out |= modbit::L_ALT;
    }
    if bits & modbit::R_ALT != 0 {
        out |= modbit::R_GUI;
    }
    if bits & modbit::R_GUI != 0 {
        out |= modbit::R_ALT;
    }
    out
}

/// Map one evdev key code to its HID equivalent.
pub fn map(code: u16, layout: ModifierLayout) -> Mapped {
    if let Some(bit) = layout.bit(code) {
        return Mapped::Modifier(bit);
    }
    match usage(code) {
        Some(u) => Mapped::Key(u),
        None => Mapped::Ignored,
    }
}

/// evdev code -> HID usage code, for non-modifier keys.
fn usage(code: u16) -> Option<u8> {
    use evdev_codes::*;
    Some(match code {
        // Letters
        KEY_A => 0x04, KEY_B => 0x05, KEY_C => 0x06, KEY_D => 0x07,
        KEY_E => 0x08, KEY_F => 0x09, KEY_G => 0x0A, KEY_H => 0x0B,
        KEY_I => 0x0C, KEY_J => 0x0D, KEY_K => 0x0E, KEY_L => 0x0F,
        KEY_M => 0x10, KEY_N => 0x11, KEY_O => 0x12, KEY_P => 0x13,
        KEY_Q => 0x14, KEY_R => 0x15, KEY_S => 0x16, KEY_T => 0x17,
        KEY_U => 0x18, KEY_V => 0x19, KEY_W => 0x1A, KEY_X => 0x1B,
        KEY_Y => 0x1C, KEY_Z => 0x1D,

        // Digit row
        KEY_1 => 0x1E, KEY_2 => 0x1F, KEY_3 => 0x20, KEY_4 => 0x21,
        KEY_5 => 0x22, KEY_6 => 0x23, KEY_7 => 0x24, KEY_8 => 0x25,
        KEY_9 => 0x26, KEY_0 => 0x27,

        // Editing / whitespace
        KEY_ENTER => 0x28,
        KEY_ESC => 0x29,
        KEY_BACKSPACE => 0x2A,
        KEY_TAB => 0x2B,
        KEY_SPACE => 0x2C,

        // Punctuation
        KEY_MINUS => 0x2D,
        KEY_EQUAL => 0x2E,
        KEY_LEFTBRACE => 0x2F,
        KEY_RIGHTBRACE => 0x30,
        KEY_BACKSLASH => 0x31,
        KEY_SEMICOLON => 0x33,
        KEY_APOSTROPHE => 0x34,
        KEY_GRAVE => 0x35,
        KEY_COMMA => 0x36,
        KEY_DOT => 0x37,
        KEY_SLASH => 0x38,
        KEY_CAPSLOCK => 0x39,

        // Function row
        KEY_F1 => 0x3A, KEY_F2 => 0x3B, KEY_F3 => 0x3C, KEY_F4 => 0x3D,
        KEY_F5 => 0x3E, KEY_F6 => 0x3F, KEY_F7 => 0x40, KEY_F8 => 0x41,
        KEY_F9 => 0x42, KEY_F10 => 0x43, KEY_F11 => 0x44, KEY_F12 => 0x45,

        // Navigation cluster
        KEY_SYSRQ => 0x46,      // PrintScreen
        KEY_SCROLLLOCK => 0x47,
        KEY_PAUSE => 0x48,
        KEY_INSERT => 0x49,
        KEY_HOME => 0x4A,
        KEY_PAGEUP => 0x4B,
        KEY_DELETE => 0x4C,
        KEY_END => 0x4D,
        KEY_PAGEDOWN => 0x4E,
        KEY_RIGHT => 0x4F,
        KEY_LEFT => 0x50,
        KEY_DOWN => 0x51,
        KEY_UP => 0x52,

        // Keypad
        KEY_NUMLOCK => 0x53,
        KEY_KPSLASH => 0x54,
        KEY_KPASTERISK => 0x55,
        KEY_KPMINUS => 0x56,
        KEY_KPPLUS => 0x57,
        KEY_KPENTER => 0x58,
        KEY_KP1 => 0x59, KEY_KP2 => 0x5A, KEY_KP3 => 0x5B,
        KEY_KP4 => 0x5C, KEY_KP5 => 0x5D, KEY_KP6 => 0x5E,
        KEY_KP7 => 0x5F, KEY_KP8 => 0x60, KEY_KP9 => 0x61,
        KEY_KP0 => 0x62, KEY_KPDOT => 0x63,

        // ISO extras
        KEY_102ND => 0x64,      // the key next to left-shift on ISO boards
        KEY_COMPOSE => 0x65,    // Menu / Application

        _ => return None,
    })
}

/// The evdev codes we care about, from `linux/input-event-codes.h`.
///
/// Spelled out rather than pulled from the `evdev` crate so that this module --
/// and its tests -- build on any host, not just Linux.
#[rustfmt::skip]
#[allow(dead_code)]
pub mod evdev_codes {
    pub const KEY_ESC: u16 = 1;
    pub const KEY_1: u16 = 2;
    pub const KEY_2: u16 = 3;
    pub const KEY_3: u16 = 4;
    pub const KEY_4: u16 = 5;
    pub const KEY_5: u16 = 6;
    pub const KEY_6: u16 = 7;
    pub const KEY_7: u16 = 8;
    pub const KEY_8: u16 = 9;
    pub const KEY_9: u16 = 10;
    pub const KEY_0: u16 = 11;
    pub const KEY_MINUS: u16 = 12;
    pub const KEY_EQUAL: u16 = 13;
    pub const KEY_BACKSPACE: u16 = 14;
    pub const KEY_TAB: u16 = 15;
    pub const KEY_Q: u16 = 16;
    pub const KEY_W: u16 = 17;
    pub const KEY_E: u16 = 18;
    pub const KEY_R: u16 = 19;
    pub const KEY_T: u16 = 20;
    pub const KEY_Y: u16 = 21;
    pub const KEY_U: u16 = 22;
    pub const KEY_I: u16 = 23;
    pub const KEY_O: u16 = 24;
    pub const KEY_P: u16 = 25;
    pub const KEY_LEFTBRACE: u16 = 26;
    pub const KEY_RIGHTBRACE: u16 = 27;
    pub const KEY_ENTER: u16 = 28;
    pub const KEY_LEFTCTRL: u16 = 29;
    pub const KEY_A: u16 = 30;
    pub const KEY_S: u16 = 31;
    pub const KEY_D: u16 = 32;
    pub const KEY_F: u16 = 33;
    pub const KEY_G: u16 = 34;
    pub const KEY_H: u16 = 35;
    pub const KEY_J: u16 = 36;
    pub const KEY_K: u16 = 37;
    pub const KEY_L: u16 = 38;
    pub const KEY_SEMICOLON: u16 = 39;
    pub const KEY_APOSTROPHE: u16 = 40;
    pub const KEY_GRAVE: u16 = 41;
    pub const KEY_LEFTSHIFT: u16 = 42;
    pub const KEY_BACKSLASH: u16 = 43;
    pub const KEY_Z: u16 = 44;
    pub const KEY_X: u16 = 45;
    pub const KEY_C: u16 = 46;
    pub const KEY_V: u16 = 47;
    pub const KEY_B: u16 = 48;
    pub const KEY_N: u16 = 49;
    pub const KEY_M: u16 = 50;
    pub const KEY_COMMA: u16 = 51;
    pub const KEY_DOT: u16 = 52;
    pub const KEY_SLASH: u16 = 53;
    pub const KEY_RIGHTSHIFT: u16 = 54;
    pub const KEY_KPASTERISK: u16 = 55;
    pub const KEY_LEFTALT: u16 = 56;
    pub const KEY_SPACE: u16 = 57;
    pub const KEY_CAPSLOCK: u16 = 58;
    pub const KEY_F1: u16 = 59;
    pub const KEY_F2: u16 = 60;
    pub const KEY_F3: u16 = 61;
    pub const KEY_F4: u16 = 62;
    pub const KEY_F5: u16 = 63;
    pub const KEY_F6: u16 = 64;
    pub const KEY_F7: u16 = 65;
    pub const KEY_F8: u16 = 66;
    pub const KEY_F9: u16 = 67;
    pub const KEY_F10: u16 = 68;
    pub const KEY_NUMLOCK: u16 = 69;
    pub const KEY_SCROLLLOCK: u16 = 70;
    pub const KEY_KP7: u16 = 71;
    pub const KEY_KP8: u16 = 72;
    pub const KEY_KP9: u16 = 73;
    pub const KEY_KPMINUS: u16 = 74;
    pub const KEY_KP4: u16 = 75;
    pub const KEY_KP5: u16 = 76;
    pub const KEY_KP6: u16 = 77;
    pub const KEY_KPPLUS: u16 = 78;
    pub const KEY_KP1: u16 = 79;
    pub const KEY_KP2: u16 = 80;
    pub const KEY_KP3: u16 = 81;
    pub const KEY_KP0: u16 = 82;
    pub const KEY_KPDOT: u16 = 83;
    pub const KEY_102ND: u16 = 86;
    pub const KEY_F11: u16 = 87;
    pub const KEY_F12: u16 = 88;
    pub const KEY_KPENTER: u16 = 96;
    pub const KEY_RIGHTCTRL: u16 = 97;
    pub const KEY_KPSLASH: u16 = 98;
    pub const KEY_SYSRQ: u16 = 99;
    pub const KEY_RIGHTALT: u16 = 100;
    pub const KEY_HOME: u16 = 102;
    pub const KEY_UP: u16 = 103;
    pub const KEY_PAGEUP: u16 = 104;
    pub const KEY_LEFT: u16 = 105;
    pub const KEY_RIGHT: u16 = 106;
    pub const KEY_END: u16 = 107;
    pub const KEY_DOWN: u16 = 108;
    pub const KEY_PAGEDOWN: u16 = 109;
    pub const KEY_INSERT: u16 = 110;
    pub const KEY_DELETE: u16 = 111;
    pub const KEY_PAUSE: u16 = 119;
    pub const KEY_LEFTMETA: u16 = 125;
    pub const KEY_RIGHTMETA: u16 = 126;
    pub const KEY_COMPOSE: u16 = 127;
}

#[cfg(test)]
mod tests {
    use super::evdev_codes::*;
    use super::*;

    #[test]
    fn letters_map_to_hid_usages() {
        // HID usage 0x04 is 'a', and the alphabet is contiguous from there.
        assert_eq!(map(KEY_A, ModifierLayout::default()), Mapped::Key(0x04));
        assert_eq!(map(KEY_B, ModifierLayout::default()), Mapped::Key(0x05));
        assert_eq!(map(KEY_Z, ModifierLayout::default()), Mapped::Key(0x1D));
    }

    #[test]
    fn digits_start_at_one_not_zero() {
        // The HID digit block runs 1..9 then 0, which is not the obvious order.
        assert_eq!(map(KEY_1, ModifierLayout::default()), Mapped::Key(0x1E));
        assert_eq!(map(KEY_9, ModifierLayout::default()), Mapped::Key(0x26));
        assert_eq!(map(KEY_0, ModifierLayout::default()), Mapped::Key(0x27));
    }

    /// The headline behaviour: the key physically where a Mac puts Command must
    /// produce Command. On a PC board that key is labelled Alt.
    #[test]
    fn mac_positional_puts_command_under_the_alt_key() {
        let l = ModifierLayout::MacPositional;
        assert_eq!(map(KEY_LEFTALT, l), Mapped::Modifier(modbit::L_GUI));
        assert_eq!(map(KEY_RIGHTALT, l), Mapped::Modifier(modbit::R_GUI));
    }

    /// ...and the Windows key, which sits where a Mac puts Option, gives Option.
    #[test]
    fn mac_positional_puts_option_under_the_win_key() {
        let l = ModifierLayout::MacPositional;
        assert_eq!(map(KEY_LEFTMETA, l), Mapped::Modifier(modbit::L_ALT));
        assert_eq!(map(KEY_RIGHTMETA, l), Mapped::Modifier(modbit::R_ALT));
    }

    #[test]
    fn ctrl_and_shift_are_never_swapped() {
        for l in [ModifierLayout::MacPositional, ModifierLayout::Literal] {
            assert_eq!(map(KEY_LEFTCTRL, l), Mapped::Modifier(modbit::L_CTRL));
            assert_eq!(map(KEY_RIGHTCTRL, l), Mapped::Modifier(modbit::R_CTRL));
            assert_eq!(map(KEY_LEFTSHIFT, l), Mapped::Modifier(modbit::L_SHIFT));
            assert_eq!(map(KEY_RIGHTSHIFT, l), Mapped::Modifier(modbit::R_SHIFT));
        }
    }

    #[test]
    fn literal_layout_honours_the_keycaps() {
        let l = ModifierLayout::Literal;
        assert_eq!(map(KEY_LEFTALT, l), Mapped::Modifier(modbit::L_ALT));
        assert_eq!(map(KEY_LEFTMETA, l), Mapped::Modifier(modbit::L_GUI));
    }

    #[test]
    fn swapping_alt_and_gui_is_an_involution() {
        // Applying the swap twice must be the identity, or a round-trip through
        // the config would silently corrupt the mapping.
        for bits in 0u8..=255 {
            assert_eq!(swap_alt_gui(swap_alt_gui(bits)), bits, "bits={bits:#010b}");
        }
    }

    #[test]
    fn unknown_codes_are_ignored_rather_than_guessed() {
        // Fn is swallowed by keyboard firmware; media keys live on another usage
        // page. Neither has a boot-protocol equivalent, so both must be dropped
        // rather than mapped to something plausible-but-wrong.
        assert_eq!(map(0xFFFF, ModifierLayout::default()), Mapped::Ignored);
    }

    #[test]
    fn no_two_keys_share_a_usage_code() {
        // A duplicate in the table would make two physical keys indistinguishable.
        let mut seen = std::collections::HashMap::new();
        for code in 0u16..256 {
            if let Mapped::Key(u) = map(code, ModifierLayout::default()) {
                if let Some(prev) = seen.insert(u, code) {
                    panic!("usage {u:#04x} claimed by evdev {prev} and {code}");
                }
            }
        }
    }
}
