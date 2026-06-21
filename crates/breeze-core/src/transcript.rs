//! Scrollback transcript helpers. The disk-backed append/flush/read path lives
//! in the platform layer; this module holds the pure text transforms.

/// Remove SGR sequences (`ESC '[' … 'm'`) from `s`, leaving the visible text.
///
/// Scalar scan, no regex. On `ESC` followed by `[`, everything up to and
/// including the next `m` is dropped — which also swallows any CSI sequence
/// that isn't `m`-terminated, including one that runs to the end of the string.
/// An `ESC` not followed by `[` is passed through with its following scalar.
pub fn strip_sgr(s: &str) -> String {
    // Fast path: nothing to strip if there's no escape byte.
    if !s.as_bytes().contains(&0x1b) {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    let mut pending = it.next();
    while let Some(c) = pending {
        if c == '\u{1b}' {
            let next = it.next();
            if next == Some('[') {
                // Consume up to and including the final 'm'.
                let mut inner = it.next();
                while let Some(ic) = inner {
                    if ic == 'm' {
                        break;
                    }
                    inner = it.next();
                }
                pending = it.next();
                continue;
            }
            out.push(c);
            if let Some(n) = next {
                out.push(n);
            }
            pending = it.next();
            continue;
        }
        out.push(c);
        pending = it.next();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_escape_is_unchanged() {
        assert_eq!(strip_sgr("plain text 123"), "plain text 123");
    }

    #[test]
    fn strips_color_run() {
        // red "hi" then reset
        assert_eq!(strip_sgr("\u{1b}[31mhi\u{1b}[0m"), "hi");
    }

    #[test]
    fn strips_multiple_sequences_in_line() {
        let s = "\u{1b}[1;32mok\u{1b}[0m done \u{1b}[33m!\u{1b}[0m";
        assert_eq!(strip_sgr(s), "ok done !");
    }

    #[test]
    fn esc_not_bracket_passes_through() {
        // ESC 'M' (reverse index) is not a CSI — keep both scalars.
        assert_eq!(strip_sgr("a\u{1b}Mb"), "a\u{1b}Mb");
    }

    #[test]
    fn unterminated_csi_is_swallowed_to_end() {
        // No 'm' anywhere after ESC '[' → everything after it is consumed.
        assert_eq!(strip_sgr("keep\u{1b}[31 drop and rest"), "keep");
    }

    #[test]
    fn non_m_csi_consumed_until_next_m() {
        // A cursor-move CSI (ends in 'H') is eaten greedily until the next 'm',
        // taking the text in between with it.
        assert_eq!(strip_sgr("a\u{1b}[2HXX\u{1b}[0mb"), "ab");
    }

    #[test]
    fn trailing_lone_esc_is_kept() {
        assert_eq!(strip_sgr("x\u{1b}"), "x\u{1b}");
    }
}
