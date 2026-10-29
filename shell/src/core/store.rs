//! Upstream's data files (spec 02 §1): settings.json, presets.json, profiles.json (read, cached by mtime) and
//! history.json (written only). Everything is a `serde_json::Value` with preserved key order, so writing one field
//! never changes the others. Writes re-read the file first, change only what the caller asks for, and go through
//! `.tmp` + rename. While upstream's hosted Filee is running (it overwrites whole files from memory and never
//! re-reads them) writes wait in a queue and are replayed on fresh data when it has ended.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::Value;

use crate::core::{hosted, util};
use crate::{info, warn};

/// The schema this build knows (upstream `AppSettings.CurrentSchemaVersion`).
pub const SCHEMA_VERSION: i64 = 54;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataFile {
    Settings,
    Presets,
    Profiles,
    History,
}

impl DataFile {
    pub fn file_name(self) -> &'static str {
        match self {
            DataFile::Settings => "settings.json",
            DataFile::Presets => "presets.json",
            DataFile::Profiles => "profiles.json",
            DataFile::History => "history.json",
        }
    }

    pub fn path(self) -> PathBuf {
        crate::paths::data_dir().join(self.file_name())
    }
}

/// An edit of one file's JSON: returns true when it changed something (then the file is written).
pub type Edit = Arc<dyn Fn(&mut Value) -> bool + Send + Sync>;

type Stamp = Option<(u64, SystemTime)>;

#[derive(Default)]
struct State {
    settings: Value,
    presets: Value,
    profiles: Value,
    stamps: [Stamp; 3],
    loaded: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    settings: Value::Null,
    presets: Value::Null,
    profiles: Value::Null,
    stamps: [None, None, None],
    loaded: false,
});
/// Serializes read-modify-write cycles.
static WRITE_LOCK: Mutex<()> = Mutex::new(());
static PENDING: Mutex<Vec<(DataFile, &'static str, Edit)>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn stamp_of(file: DataFile) -> Stamp {
    let meta = std::fs::metadata(file.path()).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn index(file: DataFile) -> usize {
    match file {
        DataFile::Settings => 0,
        DataFile::Presets => 1,
        DataFile::Profiles => 2,
        DataFile::History => 3,
    }
}

fn empty_for(file: DataFile) -> Value {
    match file {
        DataFile::Settings => Value::Object(Default::default()),
        _ => Value::Array(Vec::new()),
    }
}

/// Reads the three files (and starts building the formats table). Safe to call again.
pub fn load() {
    // An older schema is migrated by upstream's CLI at start; one run of a harmless command does it.
    if let Some(v) = util::read_json_file(&DataFile::Settings.path()) {
        let schema = v.get("schemaVersion").and_then(Value::as_i64).unwrap_or(SCHEMA_VERSION);
        if schema < SCHEMA_VERSION {
            info!("settings.json has schema {schema}; asking filee-cli to migrate it");
            let _ = util::hidden_command(&crate::paths::filee_cli()).arg("presets").output();
        }
    }
    seed_library_if_missing();
    {
        let mut st = lock(&STATE);
        for file in [DataFile::Settings, DataFile::Presets, DataFile::Profiles] {
            reload_file(&mut st, file);
        }
        st.loaded = true;
    }
    crate::core::formats::start_background();
}

/// Upstream keeps its built-in library in memory until something is saved: on a computer where Filee never ran there
/// is no presets.json / profiles.json and the donut would be empty. The patched `Filee.exe --hosted --seed-data`
/// writes upstream's own defaults (no window, exits at once); the 300 DPI policy then applies to them as usual.
fn seed_library_if_missing() {
    if DataFile::Presets.path().exists() && DataFile::Profiles.path().exists() {
        return;
    }
    let exe = crate::paths::filee_exe();
    let mut child = match util::hidden_command(&exe).args(["--hosted", "--seed-data"])
        .stdout(std::process::Stdio::null())
        .spawn() {
        Ok(c) => c,
        Err(e) => return warn!("Could not start {} to write the default library: {e}", exe.display()),
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return info!("No presets/profiles yet; Filee wrote its defaults ({status})");
            }
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(60) => {
                std::thread::sleep(std::time::Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                return warn!("Filee --seed-data did not finish; the donut stays empty until Filee saves its library");
            }
        }
    }
}

fn reload_file(st: &mut State, file: DataFile) {
    let stamp = stamp_of(file);
    let value = util::read_json_file(&file.path());
    let slot = match file {
        DataFile::Settings => &mut st.settings,
        DataFile::Presets => &mut st.presets,
        DataFile::Profiles => &mut st.profiles,
        DataFile::History => return,
    };
    match value {
        Some(v) => *slot = v,
        // Unreadable or missing: keep what we had; upstream would use its built-in defaults, so an empty value is
        // the honest "nothing" for a first read.
        None if slot.is_null() => *slot = empty_for(file),
        None => warn!("{} could not be read; keeping the previous data", file.file_name()),
    }
    st.stamps[index(file)] = stamp;
}

/// True when a data file changed on disk since the last load (and it was reloaded). Also replays queued writes once
/// the hosted Filee has ended. Called every 1.5 s.
pub fn reload_if_changed() -> bool {
    flush_pending();
    let mut st = lock(&STATE);
    if !st.loaded {
        for file in [DataFile::Settings, DataFile::Presets, DataFile::Profiles] {
            reload_file(&mut st, file);
        }
        st.loaded = true;
        return true;
    }
    let mut changed = false;
    for file in [DataFile::Settings, DataFile::Presets, DataFile::Profiles] {
        if stamp_of(file) != st.stamps[index(file)] {
            reload_file(&mut st, file);
            changed = true;
        }
    }
    changed
}

pub fn settings() -> Value {
    lock(&STATE).settings.clone()
}

pub fn presets() -> Value {
    lock(&STATE).presets.clone()
}

pub fn profiles() -> Value {
    lock(&STATE).profiles.clone()
}

/// The preset with this id (case-insensitive, as upstream's `FindPreset`).
pub fn find_preset(id: &str) -> Option<Value> {
    lock(&STATE)
        .presets
        .as_array()?
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str).is_some_and(|i| i.eq_ignore_ascii_case(id)))
        .cloned()
}

/// Tray "pause": the one settings field Filee Rust owns.
pub fn set_paused(paused: bool) {
    mutate(
        DataFile::Settings,
        "paused",
        Arc::new(move |v| {
            if v.get("paused").and_then(Value::as_bool) == Some(paused) {
                return false;
            }
            if let Some(obj) = v.as_object_mut() {
                obj.insert("paused".into(), Value::Bool(paused));
                return true;
            }
            false
        }),
    );
}

/// Changes one data file. Applied now on freshly read data, or queued when the hosted Filee is running.
/// Returns true when it was applied (or turned out to need no change), false when it was queued or refused.
pub fn mutate(file: DataFile, label: &'static str, edit: Edit) -> bool {
    if hosted::is_running() {
        // The in-memory copy follows at once (the donut and tray must not lag); the file is written later.
        let mut st = lock(&STATE);
        match file {
            DataFile::Settings => {
                edit(&mut st.settings);
            }
            DataFile::Presets => {
                edit(&mut st.presets);
            }
            DataFile::Profiles => {
                edit(&mut st.profiles);
            }
            DataFile::History => {}
        }
        drop(st);
        let mut pending = lock(&PENDING);
        // A repeated edit with the same label replaces the older one (settings flip-flops, repeated 300 DPI passes).
        if label != "history" {
            pending.retain(|(f, l, _)| !(*f == file && *l == label));
        }
        pending.push((file, label, edit));
        info!("Upstream Filee is open: {} edit '{}' is queued", file.file_name(), label);
        return false;
    }
    apply_now(file, label, &edit)
}

fn flush_pending() {
    if lock(&PENDING).is_empty() || hosted::is_running() {
        return;
    }
    let batch: Vec<_> = std::mem::take(&mut *lock(&PENDING));
    for (file, label, edit) in batch {
        apply_now(file, label, &edit);
    }
}

/// Number of edits waiting for the hosted Filee to end.
#[allow(dead_code)] // for a status line ("n edits wait for the settings window to close")
pub fn pending_count() -> usize {
    lock(&PENDING).len()
}

fn schema_allows_writing() -> bool {
    match util::read_json_file(&DataFile::Settings.path()) {
        // No settings file yet: upstream has not run; writing a lone presets/history file is harmless but we keep
        // out of the way and let upstream create its files first.
        None => true,
        Some(v) => v.get("schemaVersion").and_then(Value::as_i64).unwrap_or(SCHEMA_VERSION) == SCHEMA_VERSION,
    }
}

fn apply_now(file: DataFile, label: &str, edit: &Edit) -> bool {
    let _guard = lock(&WRITE_LOCK);
    if !schema_allows_writing() {
        warn!("settings.json has a different schema than {SCHEMA_VERSION}; not writing {} ('{label}')", file.file_name());
        return false;
    }
    let path = file.path();
    let mut value = match util::read_json_file(&path) {
        Some(v) => v,
        None if path.exists() => {
            warn!("{} is not valid JSON; not writing '{label}'", file.file_name());
            return false;
        }
        None => empty_for(file),
    };
    if !edit(&mut value) {
        return true;
    }
    let crlf = util::file_uses_crlf(&path, !matches!(file, DataFile::Presets));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match util::atomic_write(&path, util::to_upstream_json(&value, crlf).as_bytes()) {
        Ok(()) => {
            let mut st = lock(&STATE);
            match file {
                DataFile::Settings => st.settings = value,
                DataFile::Presets => st.presets = value,
                DataFile::Profiles => st.profiles = value,
                DataFile::History => {}
            }
            if file != DataFile::History {
                st.stamps[index(file)] = stamp_of(file);
            }
            true
        }
        Err(e) => {
            warn!("Could not write {} ('{label}'): {e}", file.file_name());
            false
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};

    /// A scratch data folder under `target\core-test\unit\<name>` (never the user's data).
    pub fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("core-test").join("unit").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The project folder (parent of the crate).
    pub fn project_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    /// The data folder of an `--ignored` integration test: it must be the copy under `target\core-test`
    /// (run with `FILEE_DATA_DIR` set), never the user's `%APPDATA%\Filee`.
    pub fn isolated_data_dir() -> PathBuf {
        let dir = crate::paths::data_dir().to_path_buf();
        let allowed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("core-test");
        assert!(
            crate::core::util::is_inside(&dir, &allowed),
            "refusing to run: FILEE_DATA_DIR must point under {} (it is {})",
            allowed.display(),
            dir.display()
        );
        dir
    }

    static INTEGRATION: Mutex<()> = Mutex::new(());

    /// Integration tests share the event queue and the data folder: one at a time.
    pub fn integration_lock() -> MutexGuard<'static, ()> {
        let guard = INTEGRATION.lock().unwrap_or_else(|e| e.into_inner());
        crate::events::drain();
        guard
    }

    /// The `Job` events posted since the last drain, oldest first.
    pub fn job_events() -> Vec<crate::model::JobStatus> {
        crate::events::drain()
            .into_iter()
            .filter_map(|e| match e {
                crate::events::AppEvent::Job(s) => Some(s),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        serde_json::from_str(v).unwrap()
    }

    #[test]
    fn edit_keeps_unknown_fields_and_order() {
        // The pure part of a write: an edit on a Value must not disturb other keys.
        let mut v = s(r#"{"schemaVersion":54,"zeta":1,"paused":false,"alpha":{"x":[1,2]}}"#);
        let edit: Edit = Arc::new(|v| {
            v.as_object_mut().unwrap().insert("paused".into(), Value::Bool(true));
            true
        });
        assert!(edit(&mut v));
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["schemaVersion", "zeta", "paused", "alpha"]);
        let text = util::to_upstream_json(&v, true);
        assert!(text.starts_with("{\r\n  \"schemaVersion\": 54,\r\n  \"zeta\": 1,"));
    }

    #[test]
    fn data_files_resolve_under_the_data_dir() {
        assert!(DataFile::Settings.path().ends_with("settings.json"));
        assert!(DataFile::History.path().ends_with("history.json"));
    }

    #[test]
    fn atomic_write_and_read_back_round_trip() {
        let dir = test_support::scratch("store-roundtrip");
        let path = dir.join("x.json");
        let v = s(r#"{"a":"一二三","list":[1,2,3],"f":0.42}"#);
        util::atomic_write(&path, util::to_upstream_json(&v, false).as_bytes()).unwrap();
        assert_eq!(util::read_json_file(&path).unwrap(), v);
        assert!(!dir.join("x.json.tmp").exists());
        // Bytes on disk are ASCII only (non-ASCII escaped) and have no BOM.
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.iter().all(|b| *b < 0x80));
        // A BOM in the input is tolerated.
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(b"{\"k\":1}");
        std::fs::write(dir.join("b.json"), with_bom).unwrap();
        assert_eq!(util::read_json_file(&dir.join("b.json")).unwrap(), s(r#"{"k":1}"#));
    }

    #[test]
    fn crlf_detection_follows_the_existing_file() {
        let dir = test_support::scratch("store-crlf");
        let a = dir.join("a.json");
        let b = dir.join("b.json");
        std::fs::write(&a, "{\r\n}").unwrap();
        std::fs::write(&b, "{\n}").unwrap();
        assert!(util::file_uses_crlf(&a, false));
        assert!(!util::file_uses_crlf(&b, true));
        assert!(util::file_uses_crlf(&dir.join("missing.json"), true));
    }
}
