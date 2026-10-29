//! `filee-rust.exe --selftest`: checks an installation without showing anything (installer and update.py use
//! it). Prints one line per check to stdout, `PASS` / `FAIL` at the end; exit code 0 when everything passed.

use std::process::Command;

use crate::paths;

pub fn run() -> i32 {
    // A windows-subsystem exe started from a console prints there; a pipe (update.py) works as it is.
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    let mut failed = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("{} {name}: {detail}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failed += 1;
        }
    };

    let app = paths::app_dir();
    check("app folder", app.join("Filee.exe").exists(), app.display().to_string());
    let cli = paths::filee_cli();
    match Command::new(&cli).arg("version").output() {
        Ok(out) => check(
            "filee-cli",
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ),
        Err(e) => check("filee-cli", false, format!("{}: {e}", cli.display())),
    }
    let i18n = paths::i18n_dir();
    check("translations", i18n.join("en.json").exists(), i18n.display().to_string());
    check("data folder", true, paths::data_dir().display().to_string());

    println!("{}", if failed == 0 { "PASS" } else { "FAIL" });
    if failed == 0 { 0 } else { 1 }
}
