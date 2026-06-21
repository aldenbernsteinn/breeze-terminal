//! `breeze-platform` — per-OS backends: process suspend/resume, process
//! inspection (names, children, CPU/RSS), and the duty-cycle CPU throttle.

pub mod suspend;
pub mod proc;
pub mod throttle;
pub mod child_lifecycle;
pub mod session_manager;
pub mod scrollback;
pub mod pty;
pub mod terminal;
pub mod managed;
pub mod session_store;
pub mod config_store;
pub mod permissions;
pub mod layout_store;
pub mod power;
pub mod update;
