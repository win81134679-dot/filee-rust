//! Where things are. Installed layout (see packaging/FileeLite.iss):
//!
//! ```text
//! <install>\filee-lite.exe
//! <install>\app\Filee.exe, filee-cli.exe, …   upstream Filee (official portable + patched Filee.dll)
//! <install>\i18n\<lang>.json                  upstream's translations (copied from src/Filee.App/Assets/i18n)
//! ```
//! Data is upstream's: `%APPDATA%\Filee` (or `FILEE_DATA_DIR`, as upstream). Filee Lite's own files (log, caches)
//! are in `%LOCALAPPDATA%\FileeLite`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Folder with upstream's `Filee.exe` and `filee-cli.exe`.
/// `FILEE_LITE_APP` overrides it (development: point it at `build\stage\app`).
pub fn app_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(dir) = std::env::var_os("FILEE_LITE_APP") {
            return PathBuf::from(dir);
        }
        let beside = exe_dir().join("app");
        if beside.join("filee-cli.exe").exists() {
            return beside;
        }
        // Development from shell\target\{debug,release}: the staged copy in build\stage\app.
        for up in exe_dir().ancestors().take(5) {
            let staged = up.join("build").join("stage").join("app");
            if staged.join("filee-cli.exe").exists() {
                return staged;
            }
        }
        beside
    })
}

pub fn filee_exe() -> PathBuf {
    app_dir().join("Filee.exe")
}

pub fn filee_cli() -> PathBuf {
    app_dir().join("filee-cli.exe")
}

/// Upstream's data folder (settings.json, presets.json, profiles.json, history.json).
pub fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(dir) = std::env::var_os("FILEE_DATA_DIR") {
            return PathBuf::from(dir);
        }
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        appdata.join("Filee")
    })
}

/// Filee Lite's own folder (logs, the formats cache).
pub fn lite_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let dir = local.join("FileeLite");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

/// Upstream's translations shipped next to filee-lite.exe (`i18n\zh-TW.json` …).
pub fn i18n_dir() -> PathBuf {
    let beside = exe_dir().join("i18n");
    if beside.exists() {
        return beside;
    }
    for up in exe_dir().ancestors().take(5) {
        let src = up.join("upstream").join("src").join("Filee.App").join("Assets").join("i18n");
        if src.exists() {
            return src;
        }
    }
    beside
}
