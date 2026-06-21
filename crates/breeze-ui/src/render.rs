//! No-GPU software text renderer: rasterize text into a 32-bit (0RGB) pixel
//! buffer with cosmic-text. The window layer hands this a softbuffer surface;
//! tests hand it a plain `Vec<u32>`, so rendering is verifiable headlessly.

use breeze_vt::Cell;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};

/// 8-bit sRGB triple.
pub type Rgb = (u8, u8, u8);

/// Approximate monospace cell size (width, height) in pixels for `font_size`.
pub fn cell_size(font_size: f32) -> (f32, f32) {
    (font_size * 0.6, font_size * 1.3)
}

/// Scrollback indicator thumb `(top, height)` in pixels within a `track_h`-tall
/// viewport, or `None` at the live edge (nothing scrolled — auto-hide). `rows`
/// is the visible row count, `total_lines` the scrollback+screen depth, and
/// `offset` how many lines we're scrolled back from the bottom.
pub fn scroll_thumb(track_h: i32, rows: usize, total_lines: usize, offset: usize) -> Option<(i32, i32)> {
    let scrollable = total_lines.saturating_sub(rows);
    if scrollable == 0 || offset == 0 || track_h <= 0 {
        return None; // at the bottom, or nothing to scroll
    }
    let offset = offset.min(scrollable);
    let min_h = 12.min(track_h);
    let thumb_h = ((track_h as f32 * rows as f32 / total_lines as f32) as i32).max(min_h).min(track_h);
    // offset == scrollable → top; offset small → near bottom.
    let frac_from_top = 1.0 - (offset as f32 / scrollable as f32);
    let top = ((track_h - thumb_h) as f32 * frac_from_top) as i32;
    Some((top.clamp(0, track_h - thumb_h), thumb_h))
}

/// Columns × rows that fit a `px_w` × `px_h` window at `font_size` (min 1×1).
pub fn grid_size(px_w: u32, px_h: u32, font_size: f32) -> (u16, u16) {
    let (cw, ch) = cell_size(font_size);
    let cols = ((px_w as f32 / cw).floor() as i64).clamp(1, u16::MAX as i64) as u16;
    let rows = ((px_h as f32 / ch).floor() as i64).clamp(1, u16::MAX as i64) as u16;
    (cols, rows)
}

fn pack(c: Rgb) -> u32 {
    ((c.0 as u32) << 16) | ((c.1 as u32) << 8) | c.2 as u32
}

fn blend(dst: u32, sr: u8, sg: u8, sb: u8, a: u8) -> u32 {
    let a = a as u32;
    let inv = 255 - a;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let r = (sr as u32 * a + dr * inv) / 255;
    let g = (sg as u32 * a + dg * inv) / 255;
    let b = (sb as u32 * a + db * inv) / 255;
    (r << 16) | (g << 8) | b
}

/// Fill an axis-aligned rectangle of `pixels` with `color` (clipped to bounds).
pub fn fill_rect(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: Rgb,
) {
    let c = pack(color);
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = ((x + w).max(0) as usize).min(width);
    let y1 = ((y + h).max(0) as usize).min(height);
    for py in y0..y1 {
        let row = py * width;
        for px in x0..x1 {
            pixels[row + px] = c;
        }
    }
}

/// Fill a disc of radius `r` centered at (`cx`,`cy`) with `color` (clipped).
/// Used for the per-tab close-button circle.
pub fn fill_circle(pixels: &mut [u32], width: usize, height: usize, cx: i32, cy: i32, r: i32, color: Rgb) {
    let c = pack(color);
    let r2 = r * r;
    let y0 = (cy - r).max(0);
    let y1 = (cy + r).min(height as i32 - 1);
    for py in y0..=y1 {
        let dy = py - cy;
        let dx_max = ((r2 - dy * dy).max(0) as f64).sqrt() as i32;
        let x0 = (cx - dx_max).max(0);
        let x1 = (cx + dx_max).min(width as i32 - 1);
        let row = py as usize * width;
        for px in x0..=x1 {
            pixels[row + px as usize] = c;
        }
    }
}

/// Fill a vertical rounded "pill" capsule: a `w`-wide, `h`-tall bar at (`x`,`y`)
/// with fully-rounded ends (corner radius `w/2`). Used for the scrollbar thumb.
pub fn fill_capsule(pixels: &mut [u32], width: usize, height: usize, x: i32, y: i32, w: i32, h: i32, color: Rgb) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = (w / 2).min(h / 2);
    fill_rect(pixels, width, height, x, y + r, w, (h - 2 * r).max(0), color);
    fill_circle(pixels, width, height, x + r, y + r, r, color);
    fill_circle(pixels, width, height, x + r, y + h - 1 - r, r, color);
}

/// Draw a "split pane" icon (a rounded-ish square with a center vertical
/// divider and a small "+" in the right half) — the add-pane affordance, the
/// CPU-rendered equivalent of the original app's split-square button.
pub fn draw_pane_icon(pixels: &mut [u32], width: usize, height: usize, x: i32, y: i32, sz: i32, color: Rgb) {
    let t = (sz / 14).max(1); // stroke thickness
    // Square outline.
    fill_rect(pixels, width, height, x, y, sz, t, color); // top
    fill_rect(pixels, width, height, x, y + sz - t, sz, t, color); // bottom
    fill_rect(pixels, width, height, x, y, t, sz, color); // left
    fill_rect(pixels, width, height, x + sz - t, y, t, sz, color); // right
    // Center vertical divider (two panes).
    fill_rect(pixels, width, height, x + sz / 2 - t / 2, y, t, sz, color);
    // "+" in the right half.
    let cx = x + sz * 3 / 4;
    let cy = y + sz / 2;
    let arm = sz / 6;
    fill_rect(pixels, width, height, cx - arm, cy - t / 2, arm * 2, t, color); // horizontal
    fill_rect(pixels, width, height, cx - t / 2, cy - arm, t, arm * 2, color); // vertical
}

/// Draw a `t`-px border just inside the rectangle `(x,y,w,h)` (four edges,
/// clipped) — for outlining panels/fields. Costs four `fill_rect`s, no alloc.
#[allow(clippy::too_many_arguments)]
pub fn stroke_rect(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    t: i32,
    color: Rgb,
) {
    fill_rect(pixels, width, height, x, y, w, t, color); // top
    fill_rect(pixels, width, height, x, y + h - t, w, t, color); // bottom
    fill_rect(pixels, width, height, x, y, t, h, color); // left
    fill_rect(pixels, width, height, x + w - t, y, t, h, color); // right
}

/// Alpha-blend `color` over an axis-aligned rectangle (clipped). Used to tint
/// selected text without hiding the glyphs underneath.
#[allow(clippy::too_many_arguments)]
pub fn blend_rect(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: Rgb,
    alpha: u8,
) {
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = ((x + w).max(0) as usize).min(width);
    let y1 = ((y + h).max(0) as usize).min(height);
    for py in y0..y1 {
        let row = py * width;
        for px in x0..x1 {
            pixels[row + px] = blend(pixels[row + px], color.0, color.1, color.2, alpha);
        }
    }
}

/// Platform default monospace family. A *named* family (not the generic
/// `Family::Monospace`) so the shaper draws box-drawing / symbol codepoints from
/// the text font instead of letting a color-emoji font win the fallback.
fn default_mono_family() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some("Menlo".to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Codepoints a terminal should render as text glyphs even though Unicode gives
/// them a default *emoji* presentation — box-drawing, blocks, geometric shapes,
/// arrows, technical symbols, dingbats (✓ ✗ ● ▶ ★ …) and Powerline glyphs. We
/// append VS15 (U+FE0E) after these so the shaper picks the monospace text glyph
/// rather than a color emoji. Real emoji (plane-1, U+1F300+) are untouched.
fn wants_text_presentation(c: char) -> bool {
    matches!(c as u32,
        0x2190..=0x21FF   // arrows
        | 0x2300..=0x23FF // misc technical
        | 0x2500..=0x257F // box drawing
        | 0x2580..=0x259F // block elements
        | 0x25A0..=0x25FF // geometric shapes
        | 0x2600..=0x26FF // misc symbols
        | 0x2700..=0x27BF // dingbats
        | 0x2B00..=0x2BFF // misc symbols & arrows
        | 0xE0A0..=0xE0D4 // Powerline (private use)
    )
}

/// Owns the font system + glyph cache. Reused across frames (no per-frame
/// allocation of the heavy font state).
pub struct Renderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Pinned monospace family name (from config, else the platform default).
    font_family: Option<String>,
}

impl Default for Renderer {
    fn default() -> Self {
        Renderer::new()
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_family: default_mono_family(),
        }
    }

    /// Pin the monospace family (e.g. from `font-family` in the config). Falls
    /// back to the platform default when `family` is `None`.
    pub fn set_font_family(&mut self, family: Option<String>) {
        self.font_family = family.or_else(default_mono_family);
    }

    /// Fill `pixels` (`width`×`height`, row-major 0RGB) with `bg`, then draw
    /// `text` in `fg` using a monospace font at `font_size`.
    pub fn render_text(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        text: &str,
        font_size: f32,
        fg: Rgb,
        bg: Rgb,
    ) {
        let bg_px = pack(bg);
        for p in pixels.iter_mut() {
            *p = bg_px;
        }
        self.draw_text_at(pixels, width, height, 0, 0, width as f32, height as f32, text, font_size, fg);
    }

    /// Draw `text` into a sub-region at offset (`ox`,`oy`) of size
    /// `region_w`×`region_h`, alpha-blended over whatever is already there (no
    /// clear). Used for overlays and offset content areas.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_at(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        ox: i32,
        oy: i32,
        region_w: f32,
        region_h: f32,
        text: &str,
        font_size: f32,
        fg: Rgb,
    ) {
        self.draw_run(pixels, width, height, ox, oy, region_w, region_h, text, font_size, fg, false);
    }

    /// Like `draw_text_at`, with an explicit weight (bold for terminal SGR).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_run(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        ox: i32,
        oy: i32,
        region_w: f32,
        region_h: f32,
        text: &str,
        font_size: f32,
        fg: Rgb,
        bold: bool,
    ) {
        let metrics = Metrics::new(font_size, font_size * 1.3);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(region_w), Some(region_h));
        let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
        let fam_name = self.font_family.clone();
        let family = fam_name.as_deref().map(Family::Name).unwrap_or(Family::Monospace);
        let attrs = Attrs::new().family(family).weight(weight);
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let color = Color::rgb(fg.0, fg.1, fg.2);
        buffer.draw(&mut self.font_system, &mut self.swash_cache, color, |x, y, w, h, c| {
            let a = c.a();
            if a == 0 {
                return;
            }
            for dy in 0..h as i32 {
                for dx in 0..w as i32 {
                    let px = ox + x + dx;
                    let py = oy + y + dy;
                    if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                        continue;
                    }
                    let idx = py as usize * width + px as usize;
                    pixels[idx] = blend(pixels[idx], c.r(), c.g(), c.b(), a);
                }
            }
        });
    }

    /// Draw a colored cell grid into `pixels` at offset (`ox`,`oy`). Per row we
    /// paint background runs as rects (skipping cells already equal to
    /// `default_bg`), then draw foreground runs of equal color/weight in one
    /// shaped pass — efficient and aligned for monospace text.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_cells(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        ox: i32,
        oy: i32,
        cells: &[Vec<Cell>],
        font_size: f32,
        cw: f32,
        ch: f32,
        default_bg: Rgb,
    ) {
        let chi = ch as i32;
        for (r, row) in cells.iter().enumerate() {
            let y = oy + (r as f32 * ch) as i32;

            // Background runs.
            let mut c = 0usize;
            while c < row.len() {
                let bg = row[c].bg;
                let start = c;
                while c < row.len() && row[c].bg == bg {
                    c += 1;
                }
                if bg != default_bg {
                    let x = ox + (start as f32 * cw) as i32;
                    let w = ((c - start) as f32 * cw).ceil() as i32;
                    fill_rect(pixels, width, height, x, y, w, chi, bg);
                }
            }

            // Foreground runs of equal (fg, bold). Whitespace flushes a run so we
            // never shape trailing blanks.
            let mut run = String::new();
            let mut run_start = 0usize;
            let mut run_fg = (0u8, 0u8, 0u8);
            let mut run_bold = false;
            let mut run_underline = false;
            let flush =
                |this: &mut Self, px: &mut [u32], run: &mut String, start: usize, fg: Rgb, bold: bool, underline: bool| {
                    if run.is_empty() {
                        return;
                    }
                    let x = ox + (start as f32 * cw) as i32;
                    let w = (run.chars().count() as f32 * cw).ceil() as i32 + cw as i32;
                    this.draw_run(px, width, height, x, y, w as f32, ch, run, font_size, fg, bold);
                    if underline {
                        let t = (ch * 0.07).max(1.0) as i32;
                        fill_rect(px, width, height, x, y + chi - t - 1, w, t, fg);
                    }
                    run.clear();
                };

            for (col, cell) in row.iter().enumerate() {
                let blank = cell.ch == ' ' || cell.ch == '\0';
                let same = !run.is_empty()
                    && cell.fg == run_fg
                    && cell.bold == run_bold
                    && cell.underline == run_underline;
                if blank || !same {
                    flush(self, pixels, &mut run, run_start, run_fg, run_bold, run_underline);
                }
                if !blank {
                    if run.is_empty() {
                        run_start = col;
                        run_fg = cell.fg;
                        run_bold = cell.bold;
                        run_underline = cell.underline;
                    }
                    run.push(cell.ch);
                    // Force text (not emoji) presentation for symbol/box/powerline
                    // glyphs a TUI expects as text. VS15 has zero advance, so the
                    // monospace cell alignment is unaffected.
                    if wants_text_presentation(cell.ch) {
                        run.push('\u{FE0E}');
                    }
                }
            }
            flush(self, pixels, &mut run, run_start, run_fg, run_bold, run_underline);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_thumb_hidden_at_bottom_visible_when_scrolled() {
        // At the live edge (offset 0) → hidden.
        assert_eq!(scroll_thumb(300, 24, 1000, 0), None);
        // No history → hidden.
        assert_eq!(scroll_thumb(300, 24, 24, 5), None);
        // Scrolled all the way up → thumb pinned to the top.
        let (top, h) = scroll_thumb(300, 24, 1000, 976).unwrap();
        assert_eq!(top, 0);
        assert!(h >= 12 && h <= 300);
        // Partially scrolled → thumb is below the top and within the track.
        let (top2, h2) = scroll_thumb(300, 24, 1000, 100).unwrap();
        assert!(top2 > 0 && top2 + h2 <= 300);
    }

    #[test]
    fn fill_circle_fills_center_not_corner() {
        let (w, h) = (40usize, 40usize);
        let bg = (0, 0, 0);
        let mut px = vec![pack(bg); w * h];
        let col = (0x40, 0x46, 0x52);
        fill_circle(&mut px, w, h, 20, 20, 8, col);
        assert_eq!(px[20 * w + 20], pack(col), "center should be filled");
        assert_eq!(px[0], pack(bg), "far corner should be untouched");
    }

    #[test]
    fn draw_cells_paints_cell_bg_and_colored_glyph() {
        let (w, h) = (120usize, 40usize);
        let default_bg = (0x0b, 0x10, 0x16);
        let mut px = vec![pack(default_bg); w * h];
        let mut r = Renderer::new();
        // One cell: 'X' in red on a green background.
        let cells = vec![vec![Cell {
            ch: 'X',
            fg: (0xff, 0x00, 0x00),
            bg: (0x00, 0x80, 0x00),
            bold: false,
            underline: false,
        }]];
        r.draw_cells(&mut px, w, h, 0, 0, &cells, 16.0, 10.0, 20.0, default_bg);
        // The cell background (green) was filled (top-left corner pixel).
        assert_eq!(px[2 * w + 1], pack((0x00, 0x80, 0x00)), "cell bg not filled");
        // At least one strongly-red glyph pixel was drawn.
        let red = px.iter().any(|&p| {
            let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
            r > 150 && g < 90 && b < 90
        });
        assert!(red, "expected red glyph pixels");
    }

    #[test]
    fn selection_highlight_paints_selected_cells_only() {
        // Mirrors the redraw highlight path: bg fill, then a sel-bg rect per
        // span from selection_spans, mapped with cell_size — no glyphs.
        use crate::select::selection_spans;
        let (w, h) = (240usize, 80usize);
        let font_size = 14.0;
        let (cw, ch) = cell_size(font_size);
        let bg = (0x0b, 0x10, 0x16);
        let sel = (0x1d, 0x3a, 0x52);
        let mut px = vec![pack(bg); w * h];

        let cols = (w as f32 / cw).floor() as usize;
        // Select cells (row 0, col 2) .. (row 0, col 5) inclusive.
        for (row, c0, c1) in selection_spans((0, 2), (0, 5), cols) {
            let x = (c0 as f32 * cw) as i32;
            let y = (row as f32 * ch) as i32;
            let rw = ((c1 - c0 + 1) as f32 * cw) as i32;
            fill_rect(&mut px, w, h, x, y, rw, ch as i32, sel);
        }

        // A pixel inside cell (row 0, col 3) is the selection color.
        let inside_x = (3.5 * cw) as usize;
        let inside_y = (0.5 * ch) as usize;
        assert_eq!(px[inside_y * w + inside_x], pack(sel), "selected cell not highlighted");
        // A pixel in cell (row 0, col 0) — outside the selection — stays bg.
        let outside_x = (0.5 * cw) as usize;
        assert_eq!(px[inside_y * w + outside_x], pack(bg), "unselected cell was painted");
    }

    #[test]
    fn text_writes_foreground_pixels() {
        let (w, h) = (200usize, 60usize);
        let mut px = vec![0u32; w * h];
        let bg = (0x0b, 0x10, 0x16);
        let fg = (0xcf, 0xe3, 0xf2);
        let mut r = Renderer::new();
        r.render_text(&mut px, w, h, "Breeze", 18.0, fg, bg);

        let bg_px = pack(bg);
        let non_bg = px.iter().filter(|&&p| p != bg_px).count();
        assert!(non_bg > 0, "expected glyph pixels to be drawn, got none");
        // Sanity: most of the buffer is still background.
        assert!(non_bg < (w * h) / 2, "unexpectedly many non-bg pixels: {non_bg}");
    }

    #[test]
    fn fill_rect_fills_only_the_region_and_clips() {
        let (w, h) = (20usize, 20usize);
        let mut px = vec![0u32; w * h];
        let red = (0xff, 0, 0);
        fill_rect(&mut px, w, h, 5, 5, 10, 10, red);
        let red_px = pack(red);
        assert_eq!(px[5 * w + 5], red_px);
        assert_eq!(px[14 * w + 14], red_px);
        assert_eq!(px[4 * w + 4], 0); // outside
        assert_eq!(px[15 * w + 15], 0); // outside
        // Out-of-bounds rect must not panic and clips.
        fill_rect(&mut px, w, h, 18, 18, 100, 100, red);
        assert_eq!(px[19 * w + 19], red_px);
    }

    #[test]
    fn grid_size_fits_window_and_clamps() {
        // 14px font → cell ~8.4 × 18.2.
        assert_eq!(grid_size(480, 312, 14.0), (57, 17));
        // Tiny window still yields at least 1×1.
        assert_eq!(grid_size(1, 1, 14.0), (1, 1));
        assert_eq!(grid_size(0, 0, 14.0), (1, 1));
    }

    #[test]
    fn empty_text_is_all_background() {
        let (w, h) = (64usize, 32usize);
        let mut px = vec![0u32; w * h];
        let bg = (10, 16, 22);
        let mut r = Renderer::new();
        r.render_text(&mut px, w, h, "", 16.0, (255, 255, 255), bg);
        let bg_px = pack(bg);
        assert!(px.iter().all(|&p| p == bg_px));
    }
}
