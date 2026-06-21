//! `breeze-ui` — cross-platform shell (windowing/tabs/splits/render).
//! No GPU; cached frost chrome. The window/render front-end is a thin layer
//! over the testable logic here and the engine in the other crates.

pub mod tabs;
pub mod render;
pub mod input;
pub mod chrome;
pub mod palette;
pub mod panes;
pub mod workspace;
pub mod confirm;
pub mod select;
