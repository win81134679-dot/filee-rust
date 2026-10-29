//! Global input → gestures (spec: docs/spec/01-trigger-donut.md §1, §3.1–3.2).
//!
//! OWNER: input module. Public API used by main.rs — keep these signatures:
//! - `start()`: install the low-level mouse/keyboard hooks on a thread of their own.
//! - `apply(settings)`: triggers, paused, excludedProcesses, donut.flipKey / flipWithWheel from settings.json.
//! - `set_watch_flip(on)`: report the flip key / wheel as `InputEvent::Flip` while a drag donut shows files.
//! - `last_cursor()`: last known cursor position (screen pixels).
//! - `file_manager_selection()`: the selected files of the foreground Explorer window / desktop (click mode).
//! - `stop()`.
//! Events go out through `crate::events::post(AppEvent::Input(..))`.
//!
//! Layout: `hook` (Win32 hooks + worker thread), `engine` (events and clock in, `InputEvent`s out), `gesture` (the
//! pure state machine), `scope` (where a gesture may start), `explorer` (click mode's COM selection reader),
//! `keys` (key / modifier / button names).

mod engine;
mod explorer;
mod gesture;
mod hook;
mod keys;
mod scope;

use std::path::PathBuf;

/// Installs the hooks and starts the worker. Safe to call twice; does nothing the second time.
pub fn start() {
    hook::start();
}

/// Reads triggers, `paused`, `excludedProcesses`, `donut.flipKey` and `donut.flipWithWheel`. May be called before
/// `start()` (the worker picks the latest settings up when it starts) and again at any time.
pub fn apply(settings: &serde_json::Value) {
    hook::apply(settings);
}

/// While on, the flip key and the wheel are reported as `InputEvent::Flip` (nothing is intercepted).
pub fn set_watch_flip(on: bool) {
    hook::set_watch_flip(on);
}

/// The last cursor position the hook saw (physical pixels); the real cursor position before the first mouse event.
pub fn last_cursor() -> (i32, i32) {
    hook::last_cursor()
}

/// The files (not folders) selected in the foreground File Explorer window (active tab) or on the desktop; empty when
/// there are none. Call on the UI (STA) thread.
pub fn file_manager_selection() -> Vec<PathBuf> {
    explorer::selection()
}

/// Removes the hooks and ends the threads.
pub fn stop() {
    hook::stop();
}
