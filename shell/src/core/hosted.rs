//! Upstream's Filee.exe started with `--hosted` (patches/hosted.patch): no tray, hook, watch folders or system
//! integration; one window or job at a time, and it ends itself when nothing is left. Used for the settings window
//! and for the donut slices that need a window (tools, merge order, "ask every time").
//!
//! The hosted instance has its own single-instance name (`Local\Filee-<hash>-lite`), so a second launch of it forwards
//! its arguments to the first and ends; nothing to manage here beyond starting it.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};

use crate::core::util;
use crate::{info, warn};

/// A started hosted Filee counts as running for this long even before it has created its mutex (.NET start-up), so
/// nothing is written into the data files while it is still reading them.
const START_GRACE: Duration = Duration::from_secs(8);
/// Command lines above this many characters go through a list file (`--convert-list`; the OS limit is 32 767).
const MAX_COMMAND_CHARS: usize = 24_000;

static SPAWNED: Mutex<Vec<(Child, Instant)>> = Mutex::new(Vec::new());
/// When the settings window was last asked for (see [`open_settings`]).
static LAST_SETTINGS: Mutex<Option<Instant>> = Mutex::new(None);
/// Asking again within this long starts nothing: a double click on the tray icon is two clicks.
const SETTINGS_REPEAT: Duration = Duration::from_secs(2);

/// The mutex name of the hosted instance: upstream's `Filee-<first 12 hex of SHA-256(key)>` + `-lite`, where the key
/// is the user name, plus `|<data folder, upper case>` when FILEE_DATA_DIR is set.
pub fn mutex_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let override_dir = std::env::var("FILEE_DATA_DIR").ok().filter(|v| !v.trim().is_empty());
    mutex_name_for(&user, override_dir.as_deref())
}

pub fn mutex_name_for(user: &str, data_dir: Option<&str>) -> String {
    let key = match data_dir {
        None => user.to_string(),
        Some(dir) => {
            let full = std::path::absolute(dir.trim()).unwrap_or_else(|_| PathBuf::from(dir.trim()));
            let text = full.to_string_lossy().into_owned();
            let text = text.trim_end_matches(['\\', '/']).to_string();
            format!("{user}|{}", text.to_uppercase())
        }
    };
    format!("Local\\Filee-{}-lite", util::sha256_hex_prefix(&key, 12).to_uppercase())
}

/// True while a hosted Filee is running (or has just been started by us). It overwrites the data files from memory,
/// so writers wait for it to end.
pub fn is_running() -> bool {
    if mutex_exists() {
        return true;
    }
    let mut spawned = SPAWNED.lock().unwrap_or_else(|e| e.into_inner());
    spawned.retain_mut(|(child, at)| matches!(child.try_wait(), Ok(None)) && at.elapsed() < START_GRACE);
    !spawned.is_empty()
}

fn mutex_exists() -> bool {
    let name = util::wide(&mutex_name());
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, windows::core::PCWSTR(name.as_ptr())) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

fn spawn(args: &[String]) {
    let exe = crate::paths::filee_exe();
    if !exe.exists() {
        warn!("Filee.exe not found at {}", exe.display());
        return;
    }
    match Command::new(&exe).args(args).current_dir(crate::paths::app_dir()).spawn() {
        Ok(child) => {
            info!("Started hosted Filee: {}", args.join(" "));
            SPAWNED.lock().unwrap_or_else(|e| e.into_inner()).push((child, Instant::now()));
        }
        Err(e) => warn!("Could not start {}: {e}", exe.display()),
    }
}

/// `Filee.exe --hosted --run=<id> <files…>`: as if the files were dropped on that preset's slice.
pub fn run(preset_id: &str, files: &[PathBuf]) {
    spawn(&run_args(preset_id, files));
}

/// The arguments for [`run`]; many files go through a list file in %TEMP% named like the Explorer menu's
/// (`Filee-convert-*.txt`, the only kind upstream deletes after reading).
pub fn run_args(preset_id: &str, files: &[PathBuf]) -> Vec<String> {
    let mut args = vec!["--hosted".to_string(), format!("--run={preset_id}")];
    let total: usize = files.iter().map(|f| f.as_os_str().len() + 3).sum::<usize>() + preset_id.len() + 40;
    if total <= MAX_COMMAND_CHARS {
        args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        return args;
    }
    let list = std::env::temp_dir().join(format!("Filee-convert-lite-{}-{}.txt", std::process::id(), util::new_job_id(files.len() as u64)));
    let text: String = files.iter().map(|f| format!("{}\r\n", f.display())).collect();
    match std::fs::write(&list, text) {
        Ok(()) => args.push("--convert-list".into()),
        Err(e) => {
            warn!("Could not write the file list {}: {e}", list.display());
            args.extend(files.iter().take(200).map(|f| f.to_string_lossy().into_owned()));
            return args;
        }
    }
    args.push(list.to_string_lossy().into_owned());
    args
}

/// `Filee.exe --hosted --settings`. A running one shows its window when started again, so a repeat within
/// [`SETTINGS_REPEAT`] (a double click) is dropped instead of starting a second 250 MB process that only hands over.
pub fn open_settings() {
    let now = Instant::now();
    {
        let mut last = LAST_SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
        if !settings_due(*last, now) {
            return;
        }
        *last = Some(now);
    }
    spawn(&["--hosted".to_string(), "--settings".to_string()]);
}

fn settings_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.saturating_duration_since(at) >= SETTINGS_REPEAT)
}

/// Upstream has no switch that opens one preset's editor, so this opens the settings window (where the donut
/// toolbar and presets are edited).
pub fn edit_preset(preset_id: &str) {
    info!("Edit preset '{preset_id}': opening upstream's settings");
    open_settings();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_click_opens_the_settings_once() {
        let t0 = Instant::now();
        assert!(settings_due(None, t0));
        assert!(!settings_due(Some(t0), t0 + Duration::from_millis(100)));
        assert!(settings_due(Some(t0), t0 + SETTINGS_REPEAT));
    }

    #[test]
    fn mutex_name_follows_upstream() {
        // sha256("User")[:12] = B512D97E7CBF (spec 02 §5.1), with the hosted suffix.
        assert_eq!(mutex_name_for("User", None), r"Local\Filee-B512D97E7CBF-lite");
        // A data folder is part of the key, upper-cased and without a trailing separator.
        assert_eq!(
            mutex_name_for("alice", Some(r"C:\Users\alice\AppData\Roaming\Filee\")),
            r"Local\Filee-A887F1A84BE2-lite"
        );
        assert_eq!(
            mutex_name_for("alice", Some(r"c:\users\alice\appdata\roaming\filee")),
            r"Local\Filee-A887F1A84BE2-lite"
        );
    }

    #[test]
    fn run_arguments() {
        let files = vec![PathBuf::from(r"C:\a.pdf"), PathBuf::from(r"C:\b c.pdf")];
        assert_eq!(run_args("merge-pdf", &files), ["--hosted", "--run=merge-pdf", r"C:\a.pdf", r"C:\b c.pdf"]);
        // Many files: a list file.
        let many: Vec<PathBuf> = (0..2000).map(|i| PathBuf::from(format!(r"C:\some\folder\with\a\long\path\file-{i}.pdf"))).collect();
        let args = run_args("merge-pdf", &many);
        assert_eq!(args[..3], ["--hosted", "--run=merge-pdf", "--convert-list"]);
        let list = PathBuf::from(&args[3]);
        assert!(list.file_name().unwrap().to_string_lossy().starts_with("Filee-convert-"));
        assert_eq!(std::fs::read_to_string(&list).unwrap().lines().count(), 2000);
        let _ = std::fs::remove_file(list);
    }

    #[test]
    fn nothing_hosted_is_running_here() {
        // No hosted Filee exists on the test machine's mutex name for a throwaway user.
        let name = util::wide(&mutex_name_for("no-such-user-for-tests", None));
        let found = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, windows::core::PCWSTR(name.as_ptr())) };
        assert!(found.is_err());
    }
}
