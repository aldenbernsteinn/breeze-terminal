//! Config parsing + data model. File IO (`load`/`set`/`path`) and applying the
//! config to a terminal view (UI) are deferred to the platform/ui layers; the
//! value logic here is parse-only and touches neither disk nor any UI toolkit.

/// sRGB color, 0–255 per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Terminal cursor style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    BlinkBlock,
    SteadyBlock,
    BlinkUnderline,
    SteadyUnderline,
    BlinkBar,
    SteadyBar,
}

impl CursorStyle {
    /// Parse an explicit style name (the canonical camelCase forms).
    pub fn from_string(name: &str) -> Option<CursorStyle> {
        match name {
            "blinkBlock" => Some(CursorStyle::BlinkBlock),
            "steadyBlock" => Some(CursorStyle::SteadyBlock),
            "blinkUnderline" => Some(CursorStyle::BlinkUnderline),
            "steadyUnderline" => Some(CursorStyle::SteadyUnderline),
            "blinkBar" => Some(CursorStyle::BlinkBar),
            "steadyBar" => Some(CursorStyle::SteadyBar),
            _ => None,
        }
    }
}

/// Parsed config. Every value optional → absent file/key leaves defaults.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BreezeConfig {
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub foreground: Option<Rgba>,
    pub background: Option<Rgba>,
    pub cursor_color: Option<Rgba>,
    pub selection_background: Option<Rgba>,
    pub cursor_style: Option<CursorStyle>,
    pub copy_on_select: bool,
    pub scrollback_limit: Option<u32>,

    // Cursor blink composes with the chosen style in either key order.
    cursor_style_base: Option<String>,
    cursor_blink_requested: bool,
}

impl BreezeConfig {
    /// Parse `key = value` config text into a [`BreezeConfig`].
    pub fn parse(text: &str) -> BreezeConfig {
        let mut c = BreezeConfig::default();
        for raw in text.split('\n') {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(eq) = line.find('=') else { continue };
            let key = line[..eq].trim().to_lowercase();
            let value = strip_quotes(line[eq + 1..].trim());
            if value.is_empty() {
                continue;
            }
            c.apply(&key, &value);
        }
        c
    }

    /// Apply a single `key = value` setting.
    fn apply(&mut self, key: &str, value: &str) {
        match key {
            "font-family" => self.font_family = Some(value.to_string()),
            "font-size" => {
                if let Ok(n) = value.parse::<f64>() {
                    self.font_size = Some(n);
                }
            }
            "foreground" => self.foreground = color(value).or(self.foreground),
            "background" => self.background = color(value).or(self.background),
            "cursor-color" => self.cursor_color = color(value).or(self.cursor_color),
            "selection-background" => {
                self.selection_background = color(value).or(self.selection_background)
            }
            "cursor-style" => {
                self.cursor_style_base = Some(value.to_string());
                self.cursor_style = cursor_style(value, self.cursor_blink_requested);
            }
            "cursor-style-blink" => {
                self.cursor_blink_requested = parse_bool(value);
                if let Some(base) = self.cursor_style_base.clone() {
                    self.cursor_style = cursor_style(&base, self.cursor_blink_requested);
                }
            }
            "copy-on-select" => self.copy_on_select = parse_bool(value),
            "scrollback-limit" => {
                if let Ok(n) = value.parse::<u32>() {
                    if n > 0 {
                        self.scrollback_limit = Some(n);
                    }
                }
            }
            "theme" => self.apply_theme(value),
            _ => {} // unknown key — ignore (forward-compatible)
        }
    }

    /// Apply a named theme, filling only colors not already explicitly set.
    fn apply_theme(&mut self, name: &str) {
        if matches!(name.to_lowercase().as_str(), "ice" | "frost") {
            self.background = self.background.or_else(|| hex("#0b1016"));
            self.foreground = self.foreground.or_else(|| hex("#cfe3f2"));
            self.cursor_color = self.cursor_color.or_else(|| hex("#7fc9ff"));
            self.selection_background = self.selection_background.or_else(|| hex("#1d3a52"));
        }
    }
}

/// Strip a single pair of surrounding double quotes, if present.
fn strip_quotes(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parse a boolean config value (`true`/`yes`/`on`/`1`, case-insensitive).
pub fn parse_bool(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "true" | "yes" | "on" | "1")
}

/// Resolve a cursor-style value: an exact style name, or a shape
/// (`block`/`bar`/`beam`/`underline`/`under`) combined with the blink flag.
pub fn cursor_style(name: &str, blink: bool) -> Option<CursorStyle> {
    if let Some(exact) = CursorStyle::from_string(name) {
        return Some(exact);
    }
    match name.to_lowercase().as_str() {
        "block" => Some(if blink { CursorStyle::BlinkBlock } else { CursorStyle::SteadyBlock }),
        "bar" | "beam" => Some(if blink { CursorStyle::BlinkBar } else { CursorStyle::SteadyBar }),
        "underline" | "under" => {
            Some(if blink { CursorStyle::BlinkUnderline } else { CursorStyle::SteadyUnderline })
        }
        _ => None,
    }
}

/// Parse a color value. Only `#`-prefixed hex is accepted.
pub fn color(v: &str) -> Option<Rgba> {
    if v.starts_with('#') {
        hex(v)
    } else {
        None
    }
}

/// Parse a `#rgb` or `#rrggbb` hex color.
pub fn hex(v: &str) -> Option<Rgba> {
    let s = v.strip_prefix('#').unwrap_or(v).to_lowercase();
    let chars: Vec<char> = s.chars().collect();
    match chars.len() {
        3 => {
            let r = chars[0].to_digit(16)? as u8;
            let g = chars[1].to_digit(16)? as u8;
            let b = chars[2].to_digit(16)? as u8;
            // `#rgb`: each nibble expands to a byte (`n*17`, so `f` → 255).
            Some(Rgba { r: r * 17, g: g * 17, b: b * 17, a: 255 })
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Rgba { r, g, b, a: 255 })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 255 }
    }

    #[test]
    fn hex_3_and_6_digit() {
        assert_eq!(hex("#fff"), Some(rgb(255, 255, 255)));
        assert_eq!(hex("#000"), Some(rgb(0, 0, 0)));
        assert_eq!(hex("#0b1016"), Some(rgb(0x0b, 0x10, 0x16)));
        assert_eq!(hex("#7FC9FF"), Some(rgb(0x7f, 0xc9, 0xff)));
        assert_eq!(hex("#zz"), None);
        assert_eq!(hex("#12345"), None);
    }

    #[test]
    fn color_requires_hash() {
        assert_eq!(color("red"), None);
        assert_eq!(color("#fff"), Some(rgb(255, 255, 255)));
    }

    #[test]
    fn bool_truthy_set() {
        for v in ["true", "YES", "On", "1"] {
            assert!(parse_bool(v), "{v}");
        }
        for v in ["false", "no", "0", "", "maybe"] {
            assert!(!parse_bool(v), "{v}");
        }
    }

    #[test]
    fn cursor_style_names_and_blink() {
        assert_eq!(cursor_style("block", false), Some(CursorStyle::SteadyBlock));
        assert_eq!(cursor_style("block", true), Some(CursorStyle::BlinkBlock));
        assert_eq!(cursor_style("bar", false), Some(CursorStyle::SteadyBar));
        assert_eq!(cursor_style("beam", true), Some(CursorStyle::BlinkBar));
        assert_eq!(cursor_style("underline", true), Some(CursorStyle::BlinkUnderline));
        assert_eq!(cursor_style("steadyUnderline", false), Some(CursorStyle::SteadyUnderline));
        assert_eq!(cursor_style("nope", false), None);
    }

    #[test]
    fn parse_full_config() {
        let cfg = BreezeConfig::parse(
            "# comment\nfont-family = Menlo\nfont-size = 13\nbackground = #101418\ncopy-on-select = true\nscrollback-limit = 5000\ncursor-style = bar\ncursor-style-blink = on\n",
        );
        assert_eq!(cfg.font_family.as_deref(), Some("Menlo"));
        assert_eq!(cfg.font_size, Some(13.0));
        assert_eq!(cfg.background, Some(rgb(0x10, 0x14, 0x18)));
        assert!(cfg.copy_on_select);
        assert_eq!(cfg.scrollback_limit, Some(5000));
        assert_eq!(cfg.cursor_style, Some(CursorStyle::BlinkBar)); // bar + blink, any order
    }

    #[test]
    fn blink_then_style_composes_either_order() {
        let cfg = BreezeConfig::parse("cursor-style-blink = true\ncursor-style = underline\n");
        assert_eq!(cfg.cursor_style, Some(CursorStyle::BlinkUnderline));
    }

    #[test]
    fn theme_fills_only_unset_colors() {
        let cfg = BreezeConfig::parse("theme = ice\nforeground = #ffffff\n");
        assert_eq!(cfg.foreground, Some(rgb(255, 255, 255))); // explicit wins
        assert_eq!(cfg.background, Some(rgb(0x0b, 0x10, 0x16))); // theme fills
        assert_eq!(cfg.cursor_color, Some(rgb(0x7f, 0xc9, 0xff)));
    }

    #[test]
    fn unknown_keys_ignored_and_quotes_stripped() {
        let cfg = BreezeConfig::parse("font-family = \"JetBrains Mono\"\nbogus-key = whatever\n");
        assert_eq!(cfg.font_family.as_deref(), Some("JetBrains Mono"));
    }

    #[test]
    fn empty_input_is_default() {
        assert_eq!(BreezeConfig::parse(""), BreezeConfig::default());
    }
}
