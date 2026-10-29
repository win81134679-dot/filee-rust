//! Filee Rust updates itself (2026-10-29: 「以後靠程式自動抓取repo最新exe自動靜默更新」; a tray item checks by hand;
//! every machine has an id so a release can go to some machines only).
//!
//! Once a day (and from the tray's "check for updates") filee-rust.exe runs `Filee.exe --hosted --self-update`
//! (patches/hosted.patch: no window): it reads the GitHub releases of [`REPO`], takes only a release whose
//! `update.json` is signed with the key in `update.pub` (next to filee-rust.exe), is newer than `version.txt` and is
//! for every machine or lists [`machine_id`]; it downloads the installer into `%LOCALAPPDATA%\FileeLite\updates`
//! (resuming) and checks size and SHA-256. Filee Rust then runs that installer silently when nothing is converting, no
//! hosted window is open, the PC is on mains power and nobody touched mouse or keyboard for 2 minutes (by hand: as soon
//! as nothing is converting). It lets go of its single-instance lock and quits; Setup puts the new files in place and
//! starts Filee Rust again, whether the install worked or not (packaging/FileeRust.iss, `/SELFUPDATE=1`).
//!
//! Silent: an automatic run that fails is only logged and tried again the next day. Not for: a development build (no
//! `version.txt` or `update.pub`), an install for all users (Program Files: Setup would need administrator rights),
//! `"autoUpdate": false` in `lite.json` (the tray item still works). State: `self-update.json`; ids: `machine-id.txt`;
//! the last run's output: `logs\self-update.log`; Setup's log: `logs\self-update-setup.log`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::core::util;
use crate::events::{self, AppEvent};
use crate::{info, warn};

/// Where releases come from (public: the repository and the public key are not secrets).
pub const REPO: &str = "win81134679-dot/filee-rust";
const DAY: u64 = 24 * 3600;
const IDLE_BEFORE_INSTALL_MS: u32 = 2 * 60 * 1000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
/// Readable ids: no 0/O, 1/I/L, U.
const ID_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";

static RUNNING: AtomicBool = AtomicBool::new(false);
static CHECKED_THIS_RUN: AtomicBool = AtomicBool::new(false);
/// The user asked (tray): install as soon as nothing is converting, without waiting for idle time or mains power.
/// Only for the installer that check found; cleared when that check ends without one and when Setup starts.
static FORCE: AtomicBool = AtomicBool::new(false);
/// Setup ran this many times for the same waiting installer and Filee Rust came back at the old version: stop.
const MAX_INSTALL_ATTEMPTS: u32 = 3;

// ------------------------------------------------------------------------------------------------- pure parts

/// `2.2.0-rust.5` (or a Filee Lite `2.2.0-lite.3`) as comparable numbers.
pub fn parse_version(text: &str) -> Option<[u32; 4]> {
    let (upstream, rev) = text.trim().split_once('-')?;
    let rev = rev.strip_prefix("rust.").or_else(|| rev.strip_prefix("lite."))?;
    let mut parts = upstream.split('.').map(|p| p.parse::<u32>().ok());
    let v = [parts.next()??, parts.next()??, parts.next()??, rev.parse().ok()?];
    parts.next().is_none().then_some(v)
}

/// `XXXX-XXXX` from 40 bits of `seed`.
pub fn format_machine_id(seed: &[u8]) -> String {
    let mut n = seed.iter().take(8).fold(0u64, |acc, &b| (acc << 8) | b as u64);
    let mut out = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        out.push(ID_ALPHABET[(n % ID_ALPHABET.len() as u64) as usize] as char);
        n /= ID_ALPHABET.len() as u64;
    }
    out
}

pub fn is_machine_id(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 9 && b[4] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || ID_ALPHABET.contains(c))
}

/// The tray item: id plus this computer's name, so a colleague can tell which machine the id belongs to.
pub fn menu_label(id: &str, computer: &str, zh: bool) -> String {
    match (zh, computer.is_empty()) {
        (true, false) => format!("本機識別碼：{id}（{computer}，點一下複製）"),
        (true, true) => format!("本機識別碼：{id}（點一下複製）"),
        (false, false) => format!("Machine id: {id} ({computer}, click to copy)"),
        (false, true) => format!("Machine id: {id} (click to copy)"),
    }
}

/// What "copy" puts on the clipboard: one line a colleague can paste into LINE, carrying everything the
/// publisher needs to file the id under a name in 工具/machines.json. Empty parts are left out.
pub fn id_card(id: &str, computer: &str, user: &str, version: &str, zh: bool) -> String {
    let (computer_k, user_k, version_k, sep) =
        if zh { ("電腦", "使用者", "版本", "，") } else { ("computer", "user", "version", ", ") };
    let parts: Vec<String> = [(computer_k, computer), (user_k, user), (version_k, version)]
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k} {v}"))
        .collect();
    let head = if zh { format!("Filee Rust 本機識別碼 {id}") } else { format!("Filee Rust machine id {id}") };
    match (parts.is_empty(), zh) {
        (true, _) => head,
        (false, true) => format!("{head}（{}）", parts.join(sep)),
        (false, false) => format!("{head} ({})", parts.join(sep)),
    }
}

/// `%COMPUTERNAME%`: shown next to the id only on this machine; never sent anywhere.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// One line of `Filee.exe --hosted --self-update`.
#[derive(Debug, PartialEq)]
pub enum Line {
    Current(String),
    Available(String),
    Progress(u64, u64),
    Ready(String, PathBuf),
    Failed(String),
    Other(String),
}

pub fn parse_line(line: &str) -> Line {
    let line = line.trim();
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    match word {
        "current" => Line::Current(rest.into()),
        "available" => Line::Available(rest.into()),
        "progress" => {
            let mut n = rest.split(' ').filter_map(|x| x.parse().ok());
            match (n.next(), n.next()) {
                (Some(a), Some(b)) => Line::Progress(a, b),
                _ => Line::Other(line.into()),
            }
        }
        "ready" => match rest.split_once(' ') {
            Some((v, path)) => Line::Ready(v.into(), PathBuf::from(path)),
            None => Line::Other(line.into()),
        },
        "failed" => Line::Failed(rest.into()),
        _ => Line::Other(line.into()),
    }
}

/// Inno Setup, silent, and `/SELFUPDATE=1` so it starts Filee Rust again at the end.
pub fn setup_args(log: &Path) -> Vec<String> {
    ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/SELFUPDATE=1"]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(format!("/LOG={}", log.display())))
        .collect()
}

/// Whether a downloaded installer may run now.
pub fn may_install(busy: bool, force: bool, idle_ms: u32, on_battery: bool) -> bool {
    !busy && (force || (idle_ms >= IDLE_BEFORE_INSTALL_MS && !on_battery))
}

/// Daily, and once after every start.
pub fn check_due(last_check: u64, now: u64, checked_this_run: bool) -> bool {
    !checked_this_run || now >= last_check.saturating_add(DAY)
}

/// Program Files (an install for all users): updating needs administrator rights.
pub fn installed_for_all_users(exe: &Path, program_files: &[PathBuf]) -> bool {
    let exe = util::path_key(exe);
    program_files.iter().any(|pf| !pf.as_os_str().is_empty() && exe.starts_with(&(util::path_key(pf) + "\\")))
}

// ------------------------------------------------------------------------------------------------- environment

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// This build's version (`version.txt` beside the exe, written by build_release.py); None for a development build.
pub fn current_version() -> Option<String> {
    let text = std::fs::read_to_string(exe_dir()?.join("version.txt")).ok()?;
    let v = text.trim().to_string();
    parse_version(&v).map(|_| v)
}

fn public_key() -> Option<PathBuf> {
    exe_dir().map(|d| d.join("update.pub")).filter(|p| p.exists())
}

/// This machine's id, made once (random, not derived from the computer or the user).
pub fn machine_id() -> String {
    let file = crate::paths::lite_dir().join("machine-id.txt");
    if let Ok(text) = std::fs::read_to_string(&file) {
        if is_machine_id(text.trim()) {
            return text.trim().to_string();
        }
    }
    let mut seed = Vec::new();
    seed.extend(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).to_le_bytes());
    seed.extend(std::process::id().to_le_bytes());
    seed.extend((&seed as *const _ as usize).to_le_bytes());
    let id = format_machine_id(&util::sha256(&seed));
    if let Err(e) = util::atomic_write(&file, format!("{id}\n").as_bytes()) {
        warn!("Could not save the machine id: {e}");
    }
    info!("Machine id {id}");
    id
}

fn auto_enabled() -> bool {
    util::read_json_file(&crate::paths::lite_dir().join("lite.json"))
        .and_then(|v| v.get("autoUpdate").and_then(Value::as_bool))
        .unwrap_or(true)
}

fn for_all_users() -> bool {
    let pf: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    std::env::current_exe().is_ok_and(|exe| installed_for_all_users(&exe, &pf))
}

fn idle_ms() -> u32 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if GetLastInputInfo(&mut info).as_bool() {
            GetTickCount().wrapping_sub(info.dwTime)
        } else {
            0
        }
    }
}

fn on_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0 }
}

fn updates_dir() -> PathBuf {
    crate::paths::lite_dir().join("updates")
}

fn state_file() -> PathBuf {
    crate::paths::lite_dir().join("self-update.json")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub last_check: u64,
    /// A verified installer waiting to be run: (version, path).
    pub ready: Option<(String, PathBuf)>,
    /// SHA-256 (hex) of that installer when it was verified: checked again right before it runs.
    pub sha256: Option<String>,
    /// How many times Setup was started for it.
    pub attempts: u32,
}

impl State {
    pub fn from_json(v: &Value) -> State {
        let ready = v.get("ready").and_then(|r| {
            Some((r.get("version")?.as_str()?.to_string(), PathBuf::from(r.get("path")?.as_str()?)))
        });
        let sha256 = v.pointer("/ready/sha256").and_then(Value::as_str).map(str::to_string);
        let attempts = v.pointer("/ready/attempts").and_then(Value::as_u64).unwrap_or(0) as u32;
        State { last_check: v.get("lastCheck").and_then(Value::as_u64).unwrap_or(0), ready, sha256, attempts }
    }

    pub fn to_json(&self) -> Value {
        let ready = self.ready.as_ref().map(|(v, p)| {
            json!({ "version": v, "path": p.to_string_lossy(), "sha256": self.sha256, "attempts": self.attempts })
        });
        json!({ "lastCheck": self.last_check, "ready": ready })
    }

    /// The waiting installer, unless Setup already ran for it [`MAX_INSTALL_ATTEMPTS`] times without effect.
    pub fn installable(&self) -> Option<(String, PathBuf)> {
        self.ready.clone().filter(|_| self.attempts < MAX_INSTALL_ATTEMPTS)
    }
}

fn sha256_hex(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    Some(util::sha256(&data).iter().map(|b| format!("{b:02x}")).collect())
}

fn load() -> State {
    util::read_json_file(&state_file()).map(|v| State::from_json(&v)).unwrap_or_default()
}

fn save(state: &State) {
    if let Err(e) = util::atomic_write(&state_file(), util::to_upstream_json(&state.to_json(), false).as_bytes()) {
        warn!("Could not save {}: {e}", state_file().display());
    }
}

/// What the tray shows for a manual check (zh-TW or English, like the rest of Filee Rust's own texts).
fn text(key: &str) -> &'static str {
    let zh = crate::ui::i18n::current().starts_with("zh");
    match (key, zh) {
        ("title", _) => "Filee Rust",
        ("checking", true) => "正在檢查更新…",
        ("checking", false) => "Checking for updates…",
        ("current", true) => "已是最新版",
        ("current", false) => "You have the latest version",
        ("downloading", true) => "找到新版，正在下載：",
        ("downloading", false) => "A new version is downloading: ",
        ("installing", true) => "正在安裝新版，Filee Rust 馬上會重新啟動：",
        ("installing", false) => "Installing the new version; Filee Rust restarts in a moment: ",
        ("after-jobs", true) => "下載好了，轉檔完成後就安裝：",
        ("after-jobs", false) => "Downloaded; it installs once the conversions are done: ",
        ("failed", true) => "檢查更新失敗：",
        ("failed", false) => "Update check failed: ",
        ("dev", true) => "這是開發版，不會自動更新",
        ("dev", false) => "This is a development build; it does not update itself",
        ("given-up", true) => "安裝新版一直沒成功，已停止重試：",
        ("given-up", false) => "Installing the new version kept failing; no more tries: ",
        ("all-users", true) => "裝在所有使用者（Program Files）時不能自動更新，請手動安裝新版",
        ("all-users", false) => "Installed for all users (Program Files): install new versions by hand",
        (_, true) => "",
        (_, false) => "",
    }
}

fn notify(manual: bool, key: &str, detail: &str) {
    if manual {
        events::post(AppEvent::Notice(text("title").into(), format!("{}{detail}", text(key))));
    }
}

// ------------------------------------------------------------------------------------------------- the run

/// At start: an installer that is now the running version (or older) is done with.
pub fn startup() {
    let _ = machine_id();
    let mut state = load();
    if let (Some((v, path)), Some(current)) = (state.ready.clone(), current_version()) {
        if parse_version(&v) <= parse_version(&current) {
            let _ = std::fs::remove_file(&path);
            state.ready = None;
            state.sha256 = None;
            state.attempts = 0;
            save(&state);
            info!("Updated to {current}");
        } else if state.attempts >= MAX_INSTALL_ATTEMPTS {
            warn!("Self-update: Setup for {v} ran {} times and {current} is still here; not trying again", state.attempts);
        }
    }
}

/// From the main window's timer (every few minutes). `busy`: a conversion or a hosted window is running.
pub fn tick(busy: bool) {
    let (Some(_), Some(_)) = (current_version(), public_key()) else { return };
    if for_all_users() {
        return;
    }
    let state = load();
    if state.ready.is_some() && state.installable().is_none() {
        return; // given up on this one; the next newer release replaces it
    }
    if let Some((version, path)) = state.installable() {
        let force = FORCE.load(Ordering::SeqCst);
        if path.exists() && (force || auto_enabled()) && may_install(busy, force, idle_ms(), on_battery()) {
            install(&version, &path, force);
        }
        return;
    }
    if auto_enabled() && check_due(state.last_check, now(), CHECKED_THIS_RUN.load(Ordering::SeqCst)) {
        check(false);
    }
}

/// The tray's "check for updates".
pub fn check_now() {
    if current_version().is_none() || public_key().is_none() {
        return notify(true, "dev", "");
    }
    if for_all_users() {
        return notify(true, "all-users", "");
    }
    FORCE.store(true, Ordering::SeqCst);
    let state = load();
    if let Some((version, path)) = state.installable().filter(|(_, p)| p.exists()) {
        return ready(version, path, true);
    }
    if let Some((version, _)) = state.ready.filter(|_| state.attempts >= MAX_INSTALL_ATTEMPTS) {
        FORCE.store(false, Ordering::SeqCst);
        return notify(true, "given-up", &version);
    }
    notify(true, "checking", "");
    // An automatic check already running reports to the tray too: it reads FORCE when it ends.
    check(true);
}

/// `Filee.exe --hosted --self-update` for this version, key, machine and download folder; output on a pipe.
pub fn command(current: &str, key: &Path, machine: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(crate::paths::filee_exe());
    command
        .args(["--hosted", "--self-update"])
        .current_dir(crate::paths::app_dir())
        .env("FILEE_RUST_UPDATE_REPO", REPO)
        .env("FILEE_RUST_CURRENT", current)
        .env("FILEE_RUST_MACHINE", machine)
        .env("FILEE_RUST_UPDATE_KEY", key)
        .env("FILEE_RUST_UPDATE_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    command
}

fn check(manual: bool) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    CHECKED_THIS_RUN.store(true, Ordering::SeqCst);
    let (Some(current), Some(key)) = (current_version(), public_key()) else {
        RUNNING.store(false, Ordering::SeqCst);
        return;
    };
    let mut command = command(&current, &key, &machine_id(), &updates_dir());
    let mut state = load();
    state.last_check = now();
    save(&state);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!("Could not start the self-update check: {e}");
            notify(manual, "failed", &e.to_string());
            RUNNING.store(false, Ordering::SeqCst);
            return;
        }
    };
    let stdout = child.stdout.take();
    std::thread::spawn(move || {
        let log_path = crate::paths::lite_dir().join("logs").join("self-update.log");
        let mut log = Vec::new();
        let mut outcome = None;
        // The tray may ask while an automatic check runs: from then on this one reports like a manual one.
        let manual = || manual || FORCE.load(Ordering::SeqCst);
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let manual = manual();
                match parse_line(&line) {
                    Line::Progress(..) => {}
                    Line::Available(v) => {
                        notify(manual, "downloading", &v);
                        log.push(line);
                    }
                    Line::Ready(v, p) => {
                        outcome = Some((v, p));
                        log.push(line);
                    }
                    Line::Current(v) => {
                        let zh = crate::ui::i18n::current().starts_with("zh");
                        notify(manual, "current", &if zh { format!("（{v}）") } else { format!(" ({v})") });
                        log.push(line);
                    }
                    Line::Failed(why) => {
                        notify(manual, "failed", &why);
                        log.push(line);
                    }
                    Line::Other(_) => log.push(line),
                }
            }
        }
        let ok = child.wait().is_ok_and(|s| s.success());
        let _ = std::fs::write(&log_path, log.join("\r\n") + "\r\n");
        RUNNING.store(false, Ordering::SeqCst);
        let manual = manual();
        match outcome {
            Some((v, p)) if ok => return ready(v, p, manual),
            _ if !ok => warn!("Self-update check failed (see {}); trying again tomorrow", log_path.display()),
            _ => {}
        }
        FORCE.store(false, Ordering::SeqCst);
    });
}

fn ready(version: String, path: PathBuf, manual: bool) {
    info!("Self-update: {version} is downloaded and verified");
    let mut state = load();
    if state.ready.as_ref().map(|(v, _)| v) != Some(&version) {
        state.attempts = 0;
    }
    state.sha256 = sha256_hex(&path);
    state.ready = Some((version.clone(), path));
    save(&state);
    if manual {
        events::post(AppEvent::InstallUpdate(version));
    }
}

/// The tray asked and the installer is there: run it now unless a conversion is running (then the timer does).
pub fn install_requested(version: &str, busy: bool) {
    match load().ready {
        Some((_, path)) if path.exists() && !busy && !for_all_users() && load().installable().is_some() => {
            install(version, &path, true)
        }
        Some(_) => notify(true, "after-jobs", version),
        None => {}
    }
}

fn install(version: &str, path: &Path, manual: bool) {
    let log = crate::paths::lite_dir().join("logs").join("self-update-setup.log");
    FORCE.store(false, Ordering::SeqCst);
    let mut state = load();
    // self-update.json is the user's to write: run only the bytes that were verified when they came down.
    if state.sha256.is_none() || sha256_hex(path) != state.sha256 {
        warn!("Self-update: {} changed since it was verified; downloading it again", path.display());
        let _ = std::fs::remove_file(path);
        state.ready = None;
        state.sha256 = None;
        save(&state);
        return;
    }
    state.attempts += 1;
    save(&state);
    info!("Self-update: installing {version} ({}), attempt {}", path.display(), state.attempts);
    notify(manual, "installing", version);
    // Setup refuses to run while the single-instance lock is held (its AppMutex): let go of it first.
    crate::single::release();
    let mut command = std::process::Command::new(path);
    command.args(setup_args(&log)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match command.spawn() {
        Ok(_) => events::post(AppEvent::Quit),
        Err(e) => {
            warn!("Could not start the installer {}: {e}", path.display());
            notify(manual, "failed", &e.to_string());
            crate::single::reacquire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_upstream_first_then_our_revision() {
        assert_eq!(parse_version("2.2.0-rust.5"), Some([2, 2, 0, 5]));
        assert_eq!(parse_version("2.2.0-lite.3"), Some([2, 2, 0, 3]));
        assert!(parse_version("2.2.0-rust.5") > parse_version("2.2.0-lite.4"));
        assert!(parse_version("2.3.0-rust.1") > parse_version("2.2.0-rust.9"));
        for bad in ["", "2.2.0", "2.2-rust.1", "2.2.0-beta.1", "2.2.0-rust.x", "2.2.0.1-rust.1"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn machine_ids_are_readable_and_checked() {
        let id = format_machine_id(&util::sha256(b"seed"));
        assert!(is_machine_id(&id), "{id}");
        assert_ne!(id, format_machine_id(&util::sha256(b"other seed")));
        assert!(!id.contains(['0', 'O', '1', 'I', 'L', 'U']));
        assert!(!is_machine_id("K7Q2-9XM0"));
        assert!(!is_machine_id("K7Q29XMA"));
        assert!(is_machine_id("K7Q2-9XMA"));
    }

    #[test]
    fn the_id_shows_and_copies_with_the_computer_it_belongs_to() {
        assert_eq!(menu_label("K7Q2-9XMA", "PC-ALICE", true), "本機識別碼：K7Q2-9XMA（PC-ALICE，點一下複製）");
        assert_eq!(menu_label("K7Q2-9XMA", "", true), "本機識別碼：K7Q2-9XMA（點一下複製）");
        assert_eq!(menu_label("K7Q2-9XMA", "PC-ALICE", false), "Machine id: K7Q2-9XMA (PC-ALICE, click to copy)");
        assert_eq!(
            id_card("K7Q2-9XMA", "PC-ALICE", "alice", "2.2.0-rust.5", true),
            "Filee Rust 本機識別碼 K7Q2-9XMA（電腦 PC-ALICE，使用者 alice，版本 2.2.0-rust.5）"
        );
        assert_eq!(id_card("K7Q2-9XMA", "PC-ALICE", "", "", false), "Filee Rust machine id K7Q2-9XMA (computer PC-ALICE)");
        assert_eq!(id_card("K7Q2-9XMA", "", "", "", true), "Filee Rust 本機識別碼 K7Q2-9XMA");
    }

    #[test]
    fn output_lines_are_understood() {
        assert_eq!(parse_line("current 2.2.0-rust.5"), Line::Current("2.2.0-rust.5".into()));
        assert_eq!(parse_line("available 2.2.0-rust.6"), Line::Available("2.2.0-rust.6".into()));
        assert_eq!(parse_line("progress 10 200"), Line::Progress(10, 200));
        assert_eq!(
            parse_line(r"ready 2.2.0-rust.6 C:\Users\a b\FileeLite\updates\FileeRust-2.2.0-rust.6-Setup.exe"),
            Line::Ready("2.2.0-rust.6".into(), PathBuf::from(r"C:\Users\a b\FileeLite\updates\FileeRust-2.2.0-rust.6-Setup.exe"))
        );
        assert_eq!(parse_line("failed no network"), Line::Failed("no network".into()));
        assert!(matches!(parse_line("bad-signature v9"), Line::Other(_)));
    }

    #[test]
    fn installs_only_when_nobody_is_working() {
        let two_min = IDLE_BEFORE_INSTALL_MS;
        assert!(may_install(false, false, two_min, false));
        assert!(!may_install(false, false, two_min - 1, false), "someone is at the keyboard");
        assert!(!may_install(false, false, two_min, true), "on battery");
        assert!(!may_install(true, false, two_min, false), "converting");
        // Asked for by hand: right away, but never in the middle of a conversion.
        assert!(may_install(false, true, 0, true));
        assert!(!may_install(true, true, 0, false));
    }

    #[test]
    fn checks_after_start_then_daily() {
        assert!(check_due(0, 1_000_000, false));
        assert!(check_due(999_999, 1_000_000, false), "first check after a start");
        assert!(!check_due(1_000_000, 1_000_000 + DAY - 1, true));
        assert!(check_due(1_000_000, 1_000_000 + DAY, true));
    }

    #[test]
    fn program_files_is_an_install_for_all_users() {
        let pf = [PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)"), PathBuf::new()];
        assert!(installed_for_all_users(Path::new(r"C:\Program Files\Filee Rust\filee-rust.exe"), &pf));
        assert!(installed_for_all_users(Path::new(r"c:\program files (x86)\Filee Rust\filee-rust.exe"), &pf));
        assert!(!installed_for_all_users(Path::new(r"C:\Users\a\AppData\Local\Programs\Filee Rust\filee-rust.exe"), &pf));
        assert!(!installed_for_all_users(Path::new(r"C:\Program Files Extra\x.exe"), &pf));
    }

    #[test]
    fn setup_runs_silently_and_restarts_filee_rust() {
        let args = setup_args(Path::new(r"C:\logs\s.log"));
        for a in ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SELFUPDATE=1"] {
            assert!(args.iter().any(|x| x == a), "{a}");
        }
        assert_eq!(args.last().unwrap(), r"/LOG=C:\logs\s.log");
    }

    #[test]
    fn state_round_trip() {
        let s = State {
            last_check: 5,
            ready: Some(("2.2.0-rust.6".into(), PathBuf::from(r"C:\u\x.exe"))),
            sha256: Some("ab".into()),
            attempts: 2,
        };
        assert_eq!(State::from_json(&s.to_json()), s);
        assert_eq!(State::from_json(&json!({})), State::default());
        // A state file from before attempts/sha256 existed still reads.
        let old = State::from_json(&json!({"lastCheck": 1, "ready": {"version": "2.2.0-rust.6", "path": "x"}}));
        assert_eq!((old.attempts, old.sha256), (0, None));
    }

    #[test]
    fn setup_is_not_rerun_forever_for_the_same_installer() {
        let mut s = State { ready: Some(("2.2.0-rust.6".into(), PathBuf::from("x"))), ..State::default() };
        for _ in 0..MAX_INSTALL_ATTEMPTS {
            assert!(s.installable().is_some());
            s.attempts += 1;
        }
        assert_eq!(s.installable(), None);
    }
}
