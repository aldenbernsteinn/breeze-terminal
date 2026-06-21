//! Shell helpers for inserting filesystem paths into a terminal (e.g. on a file
//! drop): single-quote the path so spaces and special characters are literal.

/// Wrap `s` in single quotes for a POSIX shell, escaping any embedded single
/// quotes via the `'\''` idiom. Safe to paste as one argument.
pub fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path() {
        assert_eq!(shell_escape("/usr/bin/env"), "'/usr/bin/env'");
    }

    #[test]
    fn spaces_are_contained() {
        assert_eq!(shell_escape("/a b/c.txt"), "'/a b/c.txt'");
    }

    #[test]
    fn embedded_single_quote() {
        // it's → 'it'\''s'
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn empty_is_empty_quotes() {
        assert_eq!(shell_escape(""), "''");
    }
}
