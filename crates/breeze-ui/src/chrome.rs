//! Neutral, no-GPU "frost" chrome geometry: an opaque frosted tab bar across
//! the top. Colors are static (a dark base + a 1px top rim highlight); there is
//! no live backdrop blur, so it costs nothing per frame beyond a rect fill.

/// Tab-bar height in pixels at the given integer DPI scale (28 logical px).
pub fn tab_bar_height(scale: u32) -> i32 {
    28 * scale.max(1) as i32
}

/// Total top chrome. The pane count + add-pane control live in the tab bar
/// itself (top-right), so the content begins directly below the tab bar.
pub fn top_chrome_height(scale: u32) -> i32 {
    tab_bar_height(scale)
}

/// Frost base fill color.
pub const FROST_BASE: (u8, u8, u8) = (0x14, 0x16, 0x1c);
/// 1px top rim highlight (subtle lighter line).
pub const FROST_RIM: (u8, u8, u8) = (0x2a, 0x2e, 0x38);

/// The content rectangle (x, y, w, h) below the top chrome (tab bar + strip).
pub fn content_rect(window_w: i32, window_h: i32, scale: u32) -> (i32, i32, i32, i32) {
    let top = top_chrome_height(scale);
    (0, top, window_w.max(0), (window_h - top).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_below_the_tab_bar() {
        assert_eq!(tab_bar_height(1), 28);
        assert_eq!(top_chrome_height(1), 28);
        assert_eq!(content_rect(800, 600, 1), (0, 28, 800, 572));
        // Hi-DPI doubles the bar.
        assert_eq!(top_chrome_height(2), 56);
        // Degenerate window doesn't go negative.
        assert_eq!(content_rect(0, 10, 1), (0, 28, 0, 0));
    }
}
