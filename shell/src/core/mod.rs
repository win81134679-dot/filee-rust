//! Upstream data, jobs and system integration (spec: docs/spec/02-data-and-jobs.md, 01 §5).
//!
//! OWNER: core module. Public API used by main.rs — keep these signatures:
//! - `store::load()`, `store::reload_if_changed() -> bool`, `store::settings() -> Value`, `store::set_paused(bool)`.
//! - `profiles::faces_for(&[PathBuf]) -> Faces`: ProfileSelector + PresetAvailability.
//! - `jobs::run_cli(preset_id, files) -> u64`: filee-cli in the background; status as `AppEvent::Job`.
//!   Also `jobs::cancel(id)` (toast "cancel").
//! - `hosted::run(preset_id, files)`, `hosted::open_settings()`, `hosted::edit_preset(id)`, `hosted::is_running()`.
//! - `history::record(&JobStatus)`.
//! - `watch::apply(settings)`: watch folders.
//! - `integration::apply(settings)`: Explorer verb for files, 300 DPI presets.
//! - `engine_update::tick(busy)`: the weekly silent engine update.
//!
//! Inside: `formats` (what upstream can convert, built in the background and cached), `util` (SHA-256, JSON and time
//! helpers).

pub mod engine_update;
pub mod self_update;
pub mod formats;
pub mod history;
pub mod hosted;
pub mod integration;
pub mod jobs;
pub mod profiles;
pub mod store;
pub mod util;
pub mod watch;

#[cfg(test)]
mod integration_tests;
