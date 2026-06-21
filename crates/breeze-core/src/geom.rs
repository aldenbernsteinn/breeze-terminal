//! Toolkit-independent geometry for layout. The UI crate maps [`Rect`] ↔ the
//! platform's native rectangle type.

/// A rectangle with a **top-left origin** (y grows downward, matching the
/// window pixel buffer and pointer coordinates). `x`/`y` are the origin;
/// `width`/`height` the size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    /// Left edge.
    pub fn min_x(&self) -> f64 {
        self.x
    }

    /// Top edge (top-left origin).
    pub fn min_y(&self) -> f64 {
        self.y
    }
}
