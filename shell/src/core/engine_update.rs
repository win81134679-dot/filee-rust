//! The weekly, silent engine update (2026-10-29: 「一週一次自動更新安裝的套件確保安全性(靜默不打擾User)」).
//!
//! Once a week filee-lite.exe starts `Filee.exe --hosted --update-engines` (patches/hosted.patch: no window, no
//! single-instance lock, nothing in the data files). It brings the engines Filee downloaded and the bundled 7-Zip up
//! to the newest pinned versions; upstream's installer checks every download's SHA-256. It only starts when nothing
//! is converting, no hosted Filee window is open and the PC is not on battery, with below-normal priority and no
//! console window. Engines installed on the PC by other means (winget, an installer) are left to their own updaters.
//!
//! State: `%LOCALAPPDATA%\FileeLite\engine-update.json` (`lastSuccess` / `lastAttempt`, Unix seconds; `enabled`).
//! A failed run is tried again a day later. The last run's output: `logs\engine-update.log`.
//! Off: `"enabled": false` in that file, or `engine-update.disabled` next to filee-lite.exe (Setup writes it when
//! its "update weekly" task is unticked).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::core::util;
use crate::{info, warn};

const WEEK: u64 = 7 * 24 * 3600;
const RETRY_AFTER_FAILURE: u64 = 24 * 3600;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

static RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    pub last_success: u64,
    pub last_attempt: u64,
    pub enabled: bool,
}

impl State {
    pub fn from_json(v: &Value) -> State {
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        State {
            last_success: n("lastSuccess"),
            last_attempt: n("lastAttempt"),
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        }
    }

    pub fn to_json(self) -> Value {
        json!({ "lastSuccess": self.last_success, "lastAttempt": self.last_attempt, "enabled": self.enabled })
    }

    /// A week after the last good run, and a day after the last try.
    pub fn due(self, now: u64) -> bool {
        self.enabled
            && now >= self.last_success.saturating_add(WEEK)
            && now >= self.last_attempt.saturating_add(RETRY_AFTER_FAILURE)
    }
}

fn state_file() -> PathBuf {
    crate::paths::lite_dir().join("engine-update.json")
}

fn load() -> State {
    util::read_json_file(&state_file()).map(|v| State::from_json(&v)).unwrap_or(State { enabled: true, ..State::default() })
}

fn save(state: State) {
    let text = util::to_upstream_json(&state.to_json(), false);
    if let Err(e) = util::atomic_write(&state_file(), text.as_bytes()) {
        warn!("Could not save {}: {e}", state_file().display());
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Setup's switch: `engine-update.disabled` next to the exe.
fn disabled_by_setup() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("engine-update.disabled")))
        .is_some_and(|f| f.exists())
}

fn on_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    // ACLineStatus: 0 offline (battery), 1 online, 255 unknown (desktops without a battery report 1 or 255).
    unsafe { GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0 }
}

/// The arguments of the update run.
pub fn args() -> Vec<String> {
    vec!["--hosted".to_string(), "--update-engines".to_string()]
}

/// Called every few minutes from the main window's timer: starts the update when it is due and the PC is idle
/// enough (`busy` = a conversion or a hosted window is running).
pub fn tick(busy: bool) {
    if busy || RUNNING.load(Ordering::SeqCst) || disabled_by_setup() {
        return;
    }
    let mut state = load();
    let started = now();
    if !state.due(started) || on_battery() {
        return;
    }
    let exe = crate::paths::filee_exe();
    if !exe.exists() {
        return;
    }
    let log_path = crate::paths::lite_dir().join("logs").join("engine-update.log");
    let _ = std::fs::create_dir_all(log_path.parent().unwrap_or(crate::paths::lite_dir()));
    let Ok(log) = std::fs::File::create(&log_path) else {
        warn!("Could not write {}", log_path.display());
        return;
    };
    let mut command = std::process::Command::new(&exe);
    command
        .args(args())
        .current_dir(crate::paths::app_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::null());
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    state.last_attempt = started;
    save(state);
    match command.spawn() {
        Ok(mut child) => {
            RUNNING.store(true, Ordering::SeqCst);
            info!("Weekly engine update started (output in {})", log_path.display());
            std::thread::spawn(move || {
                let ok = child.wait().map(|s| s.success()).unwrap_or(false);
                let mut state = load();
                if ok {
                    state.last_success = now();
                    save(state);
                    info!("Weekly engine update done");
                } else {
                    warn!("Weekly engine update failed; trying again tomorrow (see engine-update.log)");
                }
                RUNNING.store(false, Ordering::SeqCst);
            });
        }
        Err(e) => warn!("Could not start the engine update: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_weekly_and_a_day_after_a_failure() {
        let never = State { enabled: true, ..State::default() };
        assert!(never.due(1_000_000));
        let fresh = State { last_success: 1_000_000, last_attempt: 1_000_000, enabled: true };
        assert!(!fresh.due(1_000_000 + WEEK - 1));
        assert!(fresh.due(1_000_000 + WEEK));
        // Failed yesterday: wait a day before trying again.
        let failed = State { last_success: 0, last_attempt: 2_000_000, enabled: true };
        assert!(!failed.due(2_000_000 + 3600));
        assert!(failed.due(2_000_000 + RETRY_AFTER_FAILURE));
        let off = State { enabled: false, ..State::default() };
        assert!(!off.due(u64::MAX));
    }

    #[test]
    fn state_round_trip_and_defaults() {
        let s = State { last_success: 5, last_attempt: 7, enabled: false };
        assert_eq!(State::from_json(&s.to_json()), s);
        // A missing or hand-written file: enabled unless it says otherwise.
        assert_eq!(State::from_json(&json!({})), State { enabled: true, ..State::default() });
        assert!(!State::from_json(&json!({ "enabled": false })).enabled);
    }

    #[test]
    fn runs_upstream_headless() {
        assert_eq!(args(), ["--hosted", "--update-engines"]);
    }
}
