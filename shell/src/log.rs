//! A small daily log in `%LOCALAPPDATA%\FileeLite\logs\filee-rust-YYYYMMDD.log`, like upstream's.
//! Use the `info!` / `warn!` / `error!` macros (`crate::info!(...)`).

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

static LOCK: Mutex<()> = Mutex::new(());

pub fn write(level: &str, module: &str, message: &str) {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = unsafe { GetLocalTime() };
    let dir = crate::paths::lite_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("filee-rust-{:04}{:02}{:02}.log", t.wYear, t.wMonth, t.wDay));
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(
            f,
            "{:02}:{:02}:{:02}.{:03} {} {}: {}",
            t.wHour, t.wMinute, t.wSecond, t.wMilliseconds, level, module, message
        );
    }
    if cfg!(debug_assertions) {
        eprintln!("{level} {module}: {message}");
    }
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write("INFO", module_path!(), &format!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write("WARN", module_path!(), &format!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", module_path!(), &format!($($arg)*)) };
}
