//! Terminal emulation: parse a PTY byte stream into a cell grid. CPU only — no
//! GPU. This is the model the UI renders and the session copies from.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, Processor};
use std::sync::{Arc, Mutex};

/// A rendered grid cell: the glyph plus its resolved 8-bit RGB colors and the
/// attributes a CPU renderer needs. `fg`/`bg` already have inverse-video and dim
/// folded in, so the renderer just paints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub bold: bool,
    pub underline: bool,
}

// House "ice" theme defaults, used when the program hasn't set an explicit color.
const ICE_FG: (u8, u8, u8) = (0xcf, 0xe3, 0xf2);
const ICE_BG: (u8, u8, u8) = (0x0b, 0x10, 0x16);
const ICE_CURSOR: (u8, u8, u8) = (0x7f, 0xc9, 0xff);

/// The 16 base ANSI colors (a warm, slightly-muted set matching the prior app).
const ANSI16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), (0x99, 0x00, 0x01), (0x00, 0xa6, 0x03), (0x99, 0x99, 0x00),
    (0x03, 0x00, 0x82), (0xb2, 0x00, 0x82), (0x00, 0xa5, 0x82), (0xbf, 0xbf, 0xbf),
    (0x8a, 0x89, 0x86), (0xe5, 0x00, 0x01), (0x00, 0xd8, 0x00), (0xe5, 0xe5, 0x00),
    (0x07, 0x00, 0xfe), (0xe5, 0x00, 0xe5), (0x00, 0xe5, 0xe5), (0xe5, 0xe5, 0xe5),
];

fn dim_rgb(c: (u8, u8, u8)) -> (u8, u8, u8) {
    (c.0 / 2, c.1 / 2, c.2 / 2)
}

/// Default RGB for a palette index when the terminal hasn't overridden it:
/// 0–15 ANSI, 16–231 the 6×6×6 cube, 232–255 the grayscale ramp, and the
/// special foreground/background/cursor + dim/bright slots.
fn default_index_rgb(idx: usize) -> (u8, u8, u8) {
    match idx {
        0..=15 => ANSI16[idx],
        16..=231 => {
            let i = idx - 16;
            let conv = |v: usize| if v == 0 { 0u8 } else { (55 + 40 * v) as u8 };
            (conv(i / 36), conv((i / 6) % 6), conv(i % 6))
        }
        232..=255 => {
            let v = (8 + 10 * (idx - 232)) as u8;
            (v, v, v)
        }
        256 => ICE_FG,        // Foreground
        257 => ICE_BG,        // Background
        258 => ICE_CURSOR,    // Cursor
        259..=266 => dim_rgb(ANSI16[idx - 259]), // DimBlack..DimWhite
        267 => ICE_FG,        // BrightForeground
        268 => dim_rgb(ICE_FG), // DimForeground
        _ => ICE_FG,
    }
}

/// Resolve a cell color to RGB: a direct spec wins; otherwise prefer the
/// terminal's live palette and fall back to the built-in defaults.
fn resolve_color(color: Color, colors: &Colors) -> (u8, u8, u8) {
    let idx = match color {
        Color::Spec(rgb) => return (rgb.r, rgb.g, rgb.b),
        Color::Named(n) => n as usize,
        Color::Indexed(i) => i as usize,
    };
    colors[idx]
        .map(|rgb| (rgb.r, rgb.g, rgb.b))
        .unwrap_or_else(|| default_index_rgb(idx))
}

/// Captures terminal-emitted events we care about (currently the title via
/// OSC 2). `send_event` takes `&self`, so state lives behind a shared mutex.
#[derive(Clone, Default)]
struct TitleSink {
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for TitleSink {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(t) => *self.title.lock().unwrap() = Some(t),
            Event::ResetTitle => *self.title.lock().unwrap() = None,
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Size {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// A live terminal: feed it PTY output, read the resulting grid.
pub struct Vt {
    term: Term<TitleSink>,
    parser: Processor,
    cols: usize,
    rows: usize,
    title: Arc<Mutex<Option<String>>>,
}

impl Vt {
    /// Default scrollback depth in lines when the config doesn't override it.
    pub const DEFAULT_HISTORY: usize = 10_000;

    pub fn new(cols: usize, rows: usize) -> Vt {
        Vt::with_history(cols, rows, Vt::DEFAULT_HISTORY)
    }

    /// Like [`Vt::new`], but with an explicit scrollback depth (`scrollback-limit`).
    pub fn with_history(cols: usize, rows: usize, history: usize) -> Vt {
        let size = Size { columns: cols, screen_lines: rows };
        let sink = TitleSink::default();
        let title = Arc::clone(&sink.title);
        let mut config = Config::default();
        config.scrolling_history = history;
        let term = Term::new(config, &size, sink);
        Vt { term, parser: Processor::new(), cols, rows, title }
    }

    /// The window/tab title last set by the program (OSC 2), if any.
    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Feed raw output bytes from the PTY.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Scroll the viewport through scrollback history: positive `delta` moves
    /// toward older lines, negative toward the bottom (live edge).
    pub fn scroll(&mut self, delta: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.scroll_display(Scroll::Delta(delta));
    }

    /// Jump the viewport back to the live (bottom) edge.
    pub fn scroll_to_bottom(&mut self) {
        use alacritty_terminal::grid::Scroll;
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Resize the grid (e.g. on window resize).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(Size { columns: cols, screen_lines: rows });
        self.cols = cols;
        self.rows = rows;
    }

    /// How far the viewport is scrolled back from the live edge, in lines
    /// (0 = at the bottom).
    pub fn scroll_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Total lines available to scroll through (scrollback + screen).
    pub fn total_lines(&self) -> usize {
        self.term.grid().total_lines()
    }

    /// All visible rows joined by newlines (trailing blank rows trimmed).
    pub fn screen_text(&self) -> String {
        let mut rows: Vec<String> = (0..self.rows).map(|r| self.row_text(r)).collect();
        while rows.last().map(|s| s.is_empty()).unwrap_or(false) {
            rows.pop();
        }
        rows.join("\n")
    }

    /// The text of a visible row (trailing blanks trimmed). Accounts for the
    /// scrollback display offset so scrolling shows older content.
    pub fn row_text(&self, row: usize) -> String {
        let grid = self.term.grid();
        let line = Line(row as i32 - grid.display_offset() as i32);
        let mut s = String::new();
        for col in 0..self.cols {
            s.push(grid[line][Column(col)].c);
        }
        s.trim_end().to_string()
    }

    /// Every visible cell with resolved colors + attributes — what a color
    /// renderer draws. One inner `Vec` per screen row (full width).
    pub fn screen_cells(&self) -> Vec<Vec<Cell>> {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let off = grid.display_offset() as i32;
        (0..self.rows)
            .map(|row| {
                let line = Line(row as i32 - off);
                (0..self.cols)
                    .map(|col| {
                        let c = &grid[line][Column(col)];
                        let mut fg = resolve_color(c.fg, colors);
                        let mut bg = resolve_color(c.bg, colors);
                        if c.flags.contains(Flags::DIM) {
                            fg = dim_rgb(fg);
                        }
                        if c.flags.contains(Flags::INVERSE) {
                            std::mem::swap(&mut fg, &mut bg);
                        }
                        Cell {
                            ch: c.c,
                            fg,
                            bg,
                            bold: c.flags.contains(Flags::BOLD),
                            underline: c.flags.contains(Flags::UNDERLINE),
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Cursor position as `(row, col)` in visible-screen coordinates, accounting
    /// for the scrollback offset. May be off-screen (row >= rows) when scrolled.
    pub fn cursor_pos(&self) -> (usize, usize) {
        let grid = self.term.grid();
        let p = grid.cursor.point;
        let row = (p.line.0 + grid.display_offset() as i32).max(0) as usize;
        (row, p.column.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_text_into_the_grid() {
        let mut vt = Vt::new(20, 5);
        vt.feed(b"hello");
        assert_eq!(vt.row_text(0), "hello");
    }

    #[test]
    fn newline_moves_to_next_row() {
        let mut vt = Vt::new(20, 5);
        vt.feed(b"one\r\ntwo");
        assert_eq!(vt.row_text(0), "one");
        assert_eq!(vt.row_text(1), "two");
    }

    #[test]
    fn captures_osc_title() {
        let mut vt = Vt::new(20, 5);
        assert_eq!(vt.title(), None);
        vt.feed(b"\x1b]2;my-title\x07");
        assert_eq!(vt.title(), Some("my-title".to_string()));
    }

    #[test]
    fn scrolling_back_shows_older_content() {
        let mut vt = Vt::new(20, 5);
        for i in 0..40 {
            vt.feed(format!("line{i}\r\n").as_bytes());
        }
        let live = vt.screen_text();
        vt.scroll(20); // up into history
        let scrolled = vt.screen_text();
        assert_ne!(live, scrolled, "scrolling back should change the visible text");
        vt.scroll_to_bottom();
        assert_eq!(vt.screen_text(), live, "back to bottom should match the live view");
    }

    #[test]
    fn scroll_offset_tracks_viewport_position() {
        let mut vt = Vt::new(20, 5);
        for i in 0..40 {
            vt.feed(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(vt.scroll_offset(), 0, "starts at the live edge");
        assert!(vt.total_lines() > 5, "history accumulated");
        vt.scroll(10);
        assert_eq!(vt.scroll_offset(), 10);
        vt.scroll_to_bottom();
        assert_eq!(vt.scroll_offset(), 0);
    }

    #[test]
    fn sgr_color_does_not_appear_as_text() {
        let mut vt = Vt::new(20, 5);
        vt.feed(b"\x1b[31mred\x1b[0m");
        assert_eq!(vt.row_text(0), "red");
    }

    #[test]
    fn screen_cells_carry_resolved_color() {
        let mut vt = Vt::new(20, 3);
        // Red foreground, then a bold cell.
        vt.feed(b"\x1b[31mR\x1b[0m\x1b[1mB\x1b[0m");
        let cells = vt.screen_cells();
        // 'R' is ANSI red (#990001).
        assert_eq!(cells[0][0].ch, 'R');
        assert_eq!(cells[0][0].fg, (0x99, 0x00, 0x01));
        // Default background is the ice theme bg.
        assert_eq!(cells[0][0].bg, (0x0b, 0x10, 0x16));
        // 'B' is bold with the default (ice) foreground.
        assert_eq!(cells[0][1].ch, 'B');
        assert!(cells[0][1].bold);
        assert_eq!(cells[0][1].fg, (0xcf, 0xe3, 0xf2));
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let mut vt = Vt::new(20, 3);
        vt.feed(b"\x1b[7mX\x1b[0m"); // reverse video
        let cells = vt.screen_cells();
        // fg/bg swapped: glyph now drawn in the bg color over the fg color.
        assert_eq!(cells[0][0].fg, (0x0b, 0x10, 0x16));
        assert_eq!(cells[0][0].bg, (0xcf, 0xe3, 0xf2));
    }
}
