//! The donut toolbar window (spec: docs/spec/01-trigger-donut.md §2, §3.3-3.4, §4).
//!
//! OWNER: donut module. Public API used by main.rs, keep these signatures:
//! - `init()`: register the window class and the OLE drop target (UI thread, after `OleInitialize`).
//! - `set_style(palette, donut_settings)`: colours and settings.json's `donut` (outerRadius, holeRatio, opacity,
//!   flipKey, flipWithWheel).
//! - `set_faces_provider(f)`: how the donut gets its slices once the files are known (drag mode learns them on
//!   DragEnter). main.rs passes a closure over `core::profiles::faces_for`.
//! - `show_for_drag(x, y)`: drag mode, before the files are known (OLE drop target, never activated).
//! - `on_drag_gesture_ended()`: the drag's mouse button went up.
//! - `show_for_files(x, y, files)`: click mode (selection gesture, Explorer verb, later launch).
//! - `flip()`: turn to the other face.
//! - `is_open()`, `close()`.
//!
//! Picks and closes go out through `crate::events::post(AppEvent::Donut(..))`.
//!
//! Layout: `geometry` and `anim` are pure math, `logic` is the state machine (hover, groups, keys, flip), `render`
//! draws it with Direct2D, `window` owns the layered window and the controller, `drop` is the OLE drop target.

mod anim;
mod drop;
mod geometry;
mod logic;
mod render;
mod window;

use std::path::PathBuf;

use crate::model::{Faces, Palette};

pub type FacesProvider = Box<dyn Fn(&[PathBuf]) -> Faces>;

pub fn init() {
    window::init();
}

/// At exit (UI thread), before COM goes away.
pub fn shutdown() {
    window::shutdown();
}

pub fn set_style(palette: &Palette, donut_settings: &serde_json::Value) {
    window::set_style(palette, donut_settings);
}

pub fn set_faces_provider(provider: FacesProvider) {
    window::set_faces_provider(provider);
}

pub fn show_for_drag(x: i32, y: i32) {
    window::show_for_drag(x, y);
}

pub fn on_drag_gesture_ended() {
    window::on_drag_gesture_ended();
}

pub fn show_for_files(x: i32, y: i32, files: Vec<PathBuf>) {
    window::show_for_files(x, y, files);
}

pub fn flip() {
    window::flip();
}

/// Part of the module's contract; main.rs does not ask yet.
#[allow(dead_code)]
pub fn is_open() -> bool {
    window::is_open()
}

#[allow(dead_code)]
pub fn close() {
    window::close();
}
