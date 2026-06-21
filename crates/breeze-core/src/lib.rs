//! `breeze-core` — platform-agnostic core of Breeze.
//!
//! Pure logic only: no OS calls, no UI. Platform seams (process scan, file IO,
//! rendering) are injected by `breeze-platform` / `breeze-ui`.

pub mod session_state;
pub mod config;
pub mod geom;
pub mod split_tree;
pub mod transcript;
pub mod proc;
pub mod process_monitor;
pub mod agents;
pub mod session;
pub mod fuzzy;
pub mod shell;
