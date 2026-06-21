//! Translate key presses into the byte sequences a terminal child expects.

use winit::keyboard::{Key, NamedKey};

/// Encode a key press into bytes to write to the PTY, or `None` if it produces
/// no input (modifier-only, unhandled named keys). `ctrl` maps letters to their
/// control codes (Ctrl-C → 0x03, etc.); `alt` (Option on macOS) enables the
/// word-wise editing bindings readline/zsh expect.
pub fn encode_key(key: &Key, ctrl: bool, alt: bool) -> Option<Vec<u8>> {
    match key {
        Key::Named(NamedKey::Enter) => Some(vec![b'\r']),
        // Option/Alt+Delete (and the Windows/Linux Ctrl+Backspace equivalent)
        // delete the previous word: ESC DEL = readline backward-kill-word.
        Key::Named(NamedKey::Backspace) => {
            if alt || ctrl {
                Some(vec![0x1b, 0x7f])
            } else {
                Some(vec![0x7f])
            }
        }
        Key::Named(NamedKey::Tab) => Some(vec![b'\t']),
        // winit reports the spacebar as a named key, not Character(" ").
        Key::Named(NamedKey::Space) => Some(vec![b' ']),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        // Word-wise movement: Option/Alt → ESC-b/ESC-f; Ctrl → CSI mod-5 (the
        // Windows/Linux convention).
        Key::Named(NamedKey::ArrowRight) => {
            if alt {
                Some(b"\x1bf".to_vec())
            } else if ctrl {
                Some(b"\x1b[1;5C".to_vec())
            } else {
                Some(b"\x1b[C".to_vec())
            }
        }
        Key::Named(NamedKey::ArrowLeft) => {
            if alt {
                Some(b"\x1bb".to_vec())
            } else if ctrl {
                Some(b"\x1b[1;5D".to_vec())
            } else {
                Some(b"\x1b[D".to_vec())
            }
        }
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Character(s) => {
            if ctrl {
                let c = s.chars().next()?;
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() {
                    // Ctrl-A..Ctrl-Z → 0x01..0x1a
                    return Some(vec![lower as u8 - b'a' + 1]);
                }
            }
            Some(s.as_bytes().to_vec())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn plain_characters() {
        assert_eq!(encode_key(&ch("a"), false, false), Some(b"a".to_vec()));
        assert_eq!(encode_key(&ch("Z"), false, false), Some(b"Z".to_vec()));
    }

    #[test]
    fn named_keys() {
        assert_eq!(encode_key(&Key::Named(NamedKey::Enter), false, false), Some(vec![b'\r']));
        assert_eq!(encode_key(&Key::Named(NamedKey::Backspace), false, false), Some(vec![0x7f]));
        assert_eq!(encode_key(&Key::Named(NamedKey::Escape), false, false), Some(vec![0x1b]));
        assert_eq!(encode_key(&Key::Named(NamedKey::Tab), false, false), Some(vec![b'\t']));
    }

    #[test]
    fn space_types_a_space() {
        assert_eq!(encode_key(&Key::Named(NamedKey::Space), false, false), Some(vec![b' ']));
    }

    #[test]
    fn arrows_are_csi_sequences() {
        assert_eq!(encode_key(&Key::Named(NamedKey::ArrowUp), false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(&Key::Named(NamedKey::ArrowLeft), false, false), Some(b"\x1b[D".to_vec()));
    }

    #[test]
    fn control_letters_map_to_control_codes() {
        assert_eq!(encode_key(&ch("c"), true, false), Some(vec![0x03])); // Ctrl-C
        assert_eq!(encode_key(&ch("d"), true, false), Some(vec![0x04])); // Ctrl-D
        assert_eq!(encode_key(&ch("A"), true, false), Some(vec![0x01])); // Ctrl-A (case-insensitive)
        // Non-letter with ctrl falls through to the literal char.
        assert_eq!(encode_key(&ch("1"), true, false), Some(b"1".to_vec()));
    }

    #[test]
    fn word_delete_backward() {
        let bs = Key::Named(NamedKey::Backspace);
        // Plain backspace deletes one char.
        assert_eq!(encode_key(&bs, false, false), Some(vec![0x7f]));
        // Option/Alt+Delete and Ctrl+Backspace both delete the previous word.
        assert_eq!(encode_key(&bs, false, true), Some(vec![0x1b, 0x7f]));
        assert_eq!(encode_key(&bs, true, false), Some(vec![0x1b, 0x7f]));
    }

    #[test]
    fn word_movement_arrows() {
        let left = Key::Named(NamedKey::ArrowLeft);
        let right = Key::Named(NamedKey::ArrowRight);
        // Option/Alt → ESC-b / ESC-f (readline word motion).
        assert_eq!(encode_key(&left, false, true), Some(b"\x1bb".to_vec()));
        assert_eq!(encode_key(&right, false, true), Some(b"\x1bf".to_vec()));
        // Ctrl → CSI modifier-5 (Windows/Linux word motion).
        assert_eq!(encode_key(&left, true, false), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(encode_key(&right, true, false), Some(b"\x1b[1;5C".to_vec()));
    }
}
