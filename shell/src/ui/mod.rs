//! Toast, tray, theme and translations (spec: docs/spec/03-toast-tray-theme.md).
//!
//! OWNER: ui module. Public API used by main.rs — keep these signatures:
//! - `theme::palette(settings)`: the Palette for settings.json's `theme` (+ theme.json, Windows light/dark).
//! - `i18n::init(language)`, `i18n::t(key)`, `i18n::format(key, args)`: upstream's `i18n\<lang>.json`.
//! - `toast::init()`, `toast::show(&JobStatus)` (create or update a card), `toast::set_palette(&Palette)`.
//! - `tray::create(hwnd)`, `tray::set_paused(bool)`, `tray::remove()`, `tray::handle_message(..)`.
//! - `toast::set_job_actions(JobActions)`: optional; the Cancel / Retry buttons appear only once core registers these.
//!   Tray commands go out as `AppEvent::OpenSettings`, a pause toggle via `crate::core::store`, `AppEvent::Quit`.

pub mod i18n;
mod render;
pub mod theme;
pub mod toast;
pub mod tray;
