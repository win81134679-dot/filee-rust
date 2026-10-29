//! Where a gesture may start (spec §1.4, upstream `TriggerService.InScope` and `WindowsPlatformServices`).
//! The rules are pure functions of class name, process name and gesture kind; the Win32 lookups feed them.

use std::collections::HashSet;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, POINT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, WindowFromPoint, GA_ROOT, GA_ROOTOWNER,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use super::gesture::{Kind, Scope, Trigger};

/// Window classes of Explorer windows and the desktop.
const FILE_MANAGER_CLASSES: [&str; 4] = ["CabinetWClass", "ExploreWClass", "Progman", "WorkerW"];

/// Other file managers, by process name (case-insensitive).
const FILE_MANAGER_PROCESSES: [&str; 10] = [
    "explorer",
    "Files",
    "dopus",
    "TOTALCMD64",
    "TOTALCMD",
    "XYplorer",
    "OneCommander",
    "Q-Dir",
    "FreeCommander",
    "FPilot",
];

const BROWSER_PROCESSES: [&str; 14] = [
    "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "whale", "arc", "zen", "librewolf", "waterfox",
    "floorp", "chromium", "yandex",
];

/// Filee's own windows never start a gesture: this program, and upstream's `Filee.exe --hosted` windows.
pub const UPSTREAM_PROCESS: &str = "Filee";

pub fn is_file_manager(class: Option<&str>, process: Option<&str>) -> bool {
    class.is_some_and(|c| FILE_MANAGER_CLASSES.contains(&c))
        || process.is_some_and(|p| FILE_MANAGER_PROCESSES.iter().any(|f| f.eq_ignore_ascii_case(p)))
}

pub fn is_desktop_class(class: &str) -> bool {
    class == "Progman" || class == "WorkerW"
}

pub fn is_browser(process: Option<&str>) -> bool {
    let Some(p) = process else { return false };
    let p = p.trim();
    let p = if p.len() >= 4 && p[p.len() - 4..].eq_ignore_ascii_case(".exe") { &p[..p.len() - 4] } else { p };
    BROWSER_PROCESSES.iter().any(|b| b.eq_ignore_ascii_case(p))
}

/// `excludedProcesses` the way upstream builds its set: each entry trimmed, every ".exe" removed (anywhere in the
/// string, case-insensitively), compared case-insensitively. Empty results are dropped.
pub fn normalize_excluded(list: &[String]) -> HashSet<String> {
    list.iter()
        .map(|s| strip_exe_everywhere(s.trim()).to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_exe_everywhere(s: &str) -> String {
    let lower = s.to_lowercase();
    // `to_lowercase` can change byte lengths for exotic characters; fall back to a plain lowercase replace then.
    if lower.len() != s.len() {
        return lower.replace(".exe", "");
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if lower[i..].starts_with(".exe") {
            i += 4;
        } else {
            let ch = s[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// What the window the gesture is judged by looks like.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowFacts {
    /// Class of the window used for the file-manager test (a desktop icon layer counts as the desktop).
    pub class: Option<String>,
    /// Process of that window, always the window's own (a desktop icon layer keeps its own process name).
    pub process: Option<String>,
    /// Class used for `is_file_manager`; differs from `class` only for desktop icon layers.
    pub fm_class: Option<String>,
}

/// The decision (`InScope` after the window is known).
/// `own` are the process names of Filee itself (any case); `excluded` comes from [`normalize_excluded`].
pub fn allows(kind: Kind, scope: Scope, window: &WindowFacts, own: &[&str], excluded: &HashSet<String>) -> bool {
    if let Some(p) = window.process.as_deref() {
        if own.iter().any(|o| o.eq_ignore_ascii_case(p)) || excluded.contains(&p.to_lowercase()) {
            return false;
        }
    }
    scope == Scope::Anywhere
        || is_file_manager(window.fm_class.as_deref().or(window.class.as_deref()), window.process.as_deref())
        || (kind == Kind::Drag && is_browser(window.process.as_deref()))
}

/// Process names that are Filee itself: this executable, and upstream's.
pub fn own_processes() -> Vec<String> {
    let mut v = vec![UPSTREAM_PROCESS.to_string()];
    if let Some(stem) = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())) {
        if !stem.eq_ignore_ascii_case(UPSTREAM_PROCESS) {
            v.push(stem);
        }
    }
    v
}

/// Settings-driven scope evaluator the worker thread owns.
pub struct ScopeChecker {
    pub excluded: HashSet<String>,
    own: Vec<String>,
}

impl ScopeChecker {
    pub fn new() -> ScopeChecker {
        ScopeChecker { excluded: HashSet::new(), own: own_processes() }
    }

    /// May `trigger` start at (x, y)? Key chords are judged by the foreground window, mouse gestures by the window
    /// under the point.
    pub fn in_scope(&self, trigger: &Trigger, x: i32, y: i32) -> bool {
        let facts = if trigger.kind == Kind::KeyChord { foreground_facts() } else { facts_at(x, y) };
        let own: Vec<&str> = self.own.iter().map(String::as_str).collect();
        allows(trigger.kind, trigger.scope, &facts, &own, &self.excluded)
    }
}

// ---- Win32 lookups ----

fn root_window_at(x: i32, y: i32) -> Option<HWND> {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return None;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        Some(if root.0.is_null() { hwnd } else { root })
    }
}

pub fn class_name_of(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// Process name without ".exe", `None` when it can't be opened (elevated or protected process).
pub fn process_name_of(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        let stem = if file.len() >= 4 && file[file.len() - 4..].eq_ignore_ascii_case(".exe") { &file[..file.len() - 4] } else { file };
        (!stem.is_empty()).then(|| stem.to_string())
    }
}

/// A top-level window owned by the desktop (a desktop icon layer) stands for the desktop in the file-manager test.
fn desktop_or_self(window: HWND) -> HWND {
    unsafe {
        let owner = GetAncestor(window, GA_ROOTOWNER);
        if !owner.0.is_null() && owner != window && class_name_of(owner).as_deref().is_some_and(is_desktop_class) {
            owner
        } else {
            window
        }
    }
}

fn facts_at(x: i32, y: i32) -> WindowFacts {
    let Some(root) = root_window_at(x, y) else { return WindowFacts::default() };
    let fm_window = desktop_or_self(root);
    WindowFacts { class: class_name_of(root), process: process_name_of(root), fm_class: class_name_of(fm_window) }
}

/// The foreground window's top-level window, `None` when there is none.
pub fn foreground_root() -> Option<HWND> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let root = GetAncestor(fg, GA_ROOT);
        Some(if root.0.is_null() { fg } else { root })
    }
}

fn foreground_facts() -> WindowFacts {
    match foreground_root() {
        Some(root) => {
            let class = class_name_of(root);
            WindowFacts { process: process_name_of(root), fm_class: class.clone(), class }
        }
        None => WindowFacts::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(class: &str, process: &str) -> WindowFacts {
        WindowFacts { class: Some(class.into()), process: Some(process.into()), fm_class: Some(class.into()) }
    }
    fn none() -> HashSet<String> {
        HashSet::new()
    }
    const OWN: [&str; 3] = ["Filee", "filee-rust", "filee-lite"]; // filee-lite: the name before 2026-10-29

    #[test]
    fn explorer_classes_and_other_file_managers_are_file_managers() {
        for c in ["CabinetWClass", "ExploreWClass", "Progman", "WorkerW"] {
            assert!(is_file_manager(Some(c), None), "{c}");
        }
        assert!(!is_file_manager(Some("cabinetwclass"), None)); // classes compare exactly
        for p in ["explorer", "EXPLORER", "dopus", "TOTALCMD64", "totalcmd", "XYplorer", "Q-Dir", "FPilot", "Files"] {
            assert!(is_file_manager(None, Some(p)), "{p}");
        }
        assert!(!is_file_manager(Some("Notepad"), Some("notepad")));
        assert!(!is_file_manager(None, None));
    }

    #[test]
    fn browsers_match_with_or_without_exe() {
        for p in ["chrome", "Chrome.EXE", "msedge", "firefox", "brave", "yandex", "librewolf"] {
            assert!(is_browser(Some(p)), "{p}");
        }
        assert!(!is_browser(Some("notepad")));
        assert!(!is_browser(None));
    }

    #[test]
    fn file_manager_scope_allows_explorer_for_every_kind() {
        let w = facts("CabinetWClass", "explorer");
        for k in [Kind::Drag, Kind::Hold, Kind::KeyChord] {
            assert!(allows(k, Scope::FileManager, &w, &OWN, &none()), "{k:?}");
        }
    }

    #[test]
    fn file_manager_scope_rejects_other_apps() {
        let w = facts("Notepad", "notepad");
        for k in [Kind::Drag, Kind::Hold, Kind::KeyChord] {
            assert!(!allows(k, Scope::FileManager, &w, &OWN, &none()), "{k:?}");
        }
    }

    #[test]
    fn browsers_are_only_for_drag() {
        let w = facts("Chrome_WidgetWin_1", "chrome");
        assert!(allows(Kind::Drag, Scope::FileManager, &w, &OWN, &none()));
        assert!(!allows(Kind::Hold, Scope::FileManager, &w, &OWN, &none()));
        assert!(!allows(Kind::KeyChord, Scope::FileManager, &w, &OWN, &none()));
        // Anywhere is anywhere
        assert!(allows(Kind::KeyChord, Scope::Anywhere, &w, &OWN, &none()));
    }

    #[test]
    fn anywhere_allows_unknown_windows_and_unknown_process() {
        assert!(allows(Kind::Drag, Scope::Anywhere, &facts("X", "y"), &OWN, &none()));
        assert!(allows(Kind::Drag, Scope::Anywhere, &WindowFacts::default(), &OWN, &none()));
        assert!(!allows(Kind::Drag, Scope::FileManager, &WindowFacts::default(), &OWN, &none()));
    }

    #[test]
    fn own_windows_are_excluded_even_over_anywhere() {
        let ex = none();
        assert!(!allows(Kind::Drag, Scope::Anywhere, &facts("DonutWnd", "filee-rust"), &OWN, &ex));
        assert!(!allows(Kind::Drag, Scope::Anywhere, &facts("Avalonia", "Filee"), &OWN, &ex));
        assert!(!allows(Kind::Drag, Scope::Anywhere, &facts("Avalonia", "FILEE"), &OWN, &ex));
    }

    #[test]
    fn excluded_processes_use_the_replace_behaviour() {
        let ex = normalize_excluded(&["  Chrome.EXE ".into(), "note.exepad".into(), "".into(), ".exe".into(), "Foo".into()]);
        assert!(ex.contains("chrome"));
        assert!(ex.contains("notepad")); // upstream's Replace(".exe", "") removes it from the middle too
        assert!(ex.contains("foo"));
        assert_eq!(ex.len(), 3);
        let w = facts("Chrome_WidgetWin_1", "Chrome");
        assert!(!allows(Kind::Drag, Scope::FileManager, &w, &OWN, &ex));
        assert!(!allows(Kind::Drag, Scope::Anywhere, &w, &OWN, &ex));
        assert!(allows(Kind::Drag, Scope::Anywhere, &facts("X", "firefox"), &OWN, &ex));
    }

    #[test]
    fn desktop_icon_layer_counts_as_desktop_but_keeps_its_own_process() {
        // a layer app's window is owned by Progman: file-manager test uses Progman, exclusion uses the layer's process
        let layer = WindowFacts {
            class: Some("IconLayerWindow".into()),
            process: Some("fences".into()),
            fm_class: Some("Progman".into()),
        };
        assert!(allows(Kind::Drag, Scope::FileManager, &layer, &OWN, &none()));
        let excluded = normalize_excluded(&["fences".into()]);
        assert!(!allows(Kind::Drag, Scope::FileManager, &layer, &OWN, &excluded));
    }

    #[test]
    fn a_process_that_could_not_be_read_is_not_excluded_but_class_still_decides() {
        let w = WindowFacts { class: Some("CabinetWClass".into()), process: None, fm_class: Some("CabinetWClass".into()) };
        assert!(allows(Kind::Hold, Scope::FileManager, &w, &OWN, &normalize_excluded(&["explorer".into()])));
    }

    #[test]
    fn exe_stripping_survives_non_ascii() {
        let ex = normalize_excluded(&["程式.exe".into(), "İx.exe".into()]);
        assert!(ex.contains("程式"));
        assert_eq!(ex.len(), 2);
    }
}
