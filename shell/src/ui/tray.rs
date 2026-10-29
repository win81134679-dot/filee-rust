//! System tray icon and its menu (spec 03 §3): left click opens settings, right click shows Open / Pause / Quit,
//! plus Filee Rust's own "check for updates" and this machine's id (click copies it; core::self_update).
//! The icon is upstream's `filee.ico`, built in; the tooltip tells the drag gesture or that triggers are paused.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use serde_json::Value;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::i18n;
use crate::events::{self, AppEvent};

/// The tray's callback message on the main window.
pub const WM_TRAY: u32 = WM_APP + 2;

const ICON_ID: u32 = 1;
const CMD_OPEN: usize = 1001;
const CMD_PAUSE: usize = 1002;
const CMD_QUIT: usize = 1003;
const CMD_CHECK_UPDATE: usize = 1004;
const CMD_COPY_ID: usize = 1005;

static ICON_FILE: &[u8] = include_bytes!("../../assets/filee.ico");

static MAIN: AtomicIsize = AtomicIsize::new(0);
static ICON: AtomicIsize = AtomicIsize::new(0);
static PAUSED: AtomicBool = AtomicBool::new(false);
static ADDED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Adds the icon. `hwnd` receives `WM_TRAY` and the menu's `WM_COMMAND`.
pub fn create(hwnd: HWND) {
    MAIN.store(hwnd.0 as isize, Ordering::SeqCst);
    unsafe {
        TASKBAR_CREATED.store(RegisterWindowMessageW(windows::core::w!("TaskbarCreated")), Ordering::SeqCst);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let size = GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16) as u32;
        match load_icon(size) {
            Some(icon) => ICON.store(icon.0 as isize, Ordering::SeqCst),
            None => crate::warn!("Tray icon could not be built from filee.ico"),
        }
    }
    add_icon();
}

/// Updates the paused state (tooltip; the menu text is built when it opens).
pub fn set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::SeqCst);
    if ADDED.load(Ordering::SeqCst) {
        notify(NIM_MODIFY);
    }
}

pub fn remove() {
    if ADDED.swap(false, Ordering::SeqCst) {
        notify(NIM_DELETE);
    }
    let icon = ICON.swap(0, Ordering::SeqCst);
    if icon != 0 {
        unsafe {
            let _ = DestroyIcon(HICON(icon as *mut _));
        }
    }
}

/// Called by main.rs for every message of the main window. Returns true when handled.
pub fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::SeqCst);
    if taskbar_created != 0 && msg == taskbar_created {
        // Explorer restarted: its tray is empty again.
        ADDED.store(false, Ordering::SeqCst);
        add_icon();
        return true;
    }
    match msg {
        WM_TRAY => {
            match (lparam.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => events::post(AppEvent::OpenSettings),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            true
        }
        // A menu item: HIWORD(wparam) == 0 and no control handle.
        WM_COMMAND if (wparam.0 >> 16) == 0 && lparam.0 == 0 => match wparam.0 & 0xFFFF {
            CMD_OPEN => {
                events::post(AppEvent::OpenSettings);
                true
            }
            CMD_PAUSE => {
                let paused = !PAUSED.load(Ordering::SeqCst);
                crate::core::store::set_paused(paused);
                set_paused(paused);
                true
            }
            CMD_QUIT => {
                events::post(AppEvent::Quit);
                true
            }
            CMD_CHECK_UPDATE => {
                crate::core::self_update::check_now();
                true
            }
            CMD_COPY_ID => {
                use crate::core::self_update as su;
                let zh = i18n::current().starts_with("zh");
                let user = std::env::var("USERNAME").unwrap_or_default();
                let version = su::current_version().unwrap_or_default();
                let card = su::id_card(&su::machine_id(), &su::computer_name(), &user, &version, zh);
                if copy_text(&card) {
                    let done = if zh { "已複製，貼給發佈者即可：" } else { "Copied; paste it to the publisher: " };
                    balloon("Filee Rust", &format!("{done}\n{card}"));
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let pause_key = if PAUSED.load(Ordering::SeqCst) { "tray.resume" } else { "tray.pause" };
        append(menu, CMD_OPEN, &i18n::t("tray.open"));
        append(menu, CMD_PAUSE, &i18n::t(pause_key));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let zh = i18n::current().starts_with("zh");
        append(menu, CMD_CHECK_UPDATE, if zh { "檢查更新" } else { "Check for updates" });
        use crate::core::self_update as su;
        append(menu, CMD_COPY_ID, &su::menu_label(&su::machine_id(), &su::computer_name(), zh));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append(menu, CMD_QUIT, &i18n::t("tray.quit"));

        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // The menu only closes on an outside click when its owner is in the foreground.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, at.x, at.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

unsafe fn append(menu: HMENU, id: usize, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR(wide.as_ptr()));
}

fn add_icon() {
    if notify(NIM_ADD) {
        ADDED.store(true, Ordering::SeqCst);
    }
}

fn notify(message: NOTIFY_ICON_MESSAGE) -> bool {
    let hwnd = HWND(MAIN.load(Ordering::SeqCst) as *mut _);
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_TIP | NIF_ICON,
        uCallbackMessage: WM_TRAY,
        hIcon: HICON(ICON.load(Ordering::SeqCst) as *mut _),
        ..Default::default()
    };
    let tip = tooltip(PAUSED.load(Ordering::SeqCst), &crate::core::store::settings());
    for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *slot = unit;
    }
    unsafe { Shell_NotifyIconW(message, &data).as_bool() }
}

use windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE;

/// A Windows notification from the tray icon (Windows 10/11 show it as a toast).
pub fn balloon(title: &str, text: &str) {
    if !ADDED.load(Ordering::SeqCst) {
        return;
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(MAIN.load(Ordering::SeqCst) as *mut _),
        uID: ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    for (slot, unit) in data.szInfoTitle.iter_mut().zip(title.encode_utf16().take(63)) {
        *slot = unit;
    }
    for (slot, unit) in data.szInfo.iter_mut().zip(text.encode_utf16().take(255)) {
        *slot = unit;
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

/// Puts `text` on the clipboard (CF_UNICODETEXT).
fn copy_text(text: &str) -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else { return false };
        let ptr = GlobalLock(mem) as *mut u16;
        if ptr.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(mem);
        let hwnd = HWND(MAIN.load(Ordering::SeqCst) as *mut _);
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// "Filee: hold Ctrl and drag files", or that the shortcuts are paused.
fn tooltip(paused: bool, settings: &Value) -> String {
    if paused {
        i18n::t("triggers.paused")
    } else {
        i18n::format("tray.tooltip", &[&gesture_text(settings)])
    }
}

/// Modifier keys of the first enabled Drag trigger as `Ctrl + Alt` (order Ctrl, Alt, Shift, Win); `—` if none.
fn gesture_text(settings: &Value) -> String {
    let modifiers = settings
        .get("triggers")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter().find(|t| {
                t.get("enabled").and_then(Value::as_bool).unwrap_or(true)
                    && t.get("kind").and_then(Value::as_str).is_some_and(|k| k.eq_ignore_ascii_case("Drag"))
            })
        })
        .and_then(|t| t.get("modifiers").and_then(Value::as_str))
        .unwrap_or("");
    let has = |name: &str| modifiers.split(',').any(|m| m.trim().eq_ignore_ascii_case(name));
    let keys: Vec<&str> = [("Ctrl", "Ctrl"), ("Alt", "Alt"), ("Shift", "Shift"), ("Meta", "Win")]
        .iter()
        .filter(|(flag, _)| has(flag))
        .map(|(_, label)| *label)
        .collect();
    if keys.is_empty() {
        "—".to_string()
    } else {
        keys.join(" + ")
    }
}

// ---- icon from the built-in .ico --------------------------------------------------------------------------

/// (offset, length) of the image in `ico` best for `want` px: the smallest one at least that big, else the largest.
fn pick_image(ico: &[u8], want: u32) -> Option<(usize, usize, u32)> {
    let u16_at = |i: usize| ico.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |i: usize| ico.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if u16_at(2)? != 1 {
        return None;
    }
    let mut images = Vec::new();
    for i in 0..u16_at(4)? {
        let entry = 6 + i * 16;
        let width = match *ico.get(entry)? {
            0 => 256,
            w => u32::from(w),
        };
        let (len, offset) = (u32_at(entry + 8)?, u32_at(entry + 12)?);
        if offset.checked_add(len)? <= ico.len() {
            images.push((offset, len, width));
        }
    }
    images
        .iter()
        .filter(|(_, _, w)| *w >= want)
        .min_by_key(|(_, _, w)| *w)
        .or_else(|| images.iter().max_by_key(|(_, _, w)| *w))
        .copied()
}

unsafe fn load_icon(size: u32) -> Option<HICON> {
    let (offset, len, width) = pick_image(ICON_FILE, size)?;
    let px = size.min(width) as i32;
    CreateIconFromResourceEx(&ICON_FILE[offset..offset + len], true, 0x0003_0000, px, px, LR_DEFAULTCOLOR).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_ico_has_the_six_upstream_sizes() {
        for (want, expect) in [(16, 16), (17, 24), (24, 24), (32, 32), (40, 48), (64, 64), (100, 256), (1000, 256)] {
            let (_, _, width) = pick_image(ICON_FILE, want).unwrap();
            assert_eq!(width, expect, "want {want}");
        }
    }

    #[test]
    fn icon_builds_at_common_tray_sizes() {
        for size in [16, 20, 24, 32] {
            let icon = unsafe { load_icon(size) }.unwrap_or_else(|| panic!("{size}"));
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }

    #[test]
    fn gesture_text_reads_the_first_enabled_drag_trigger() {
        let settings = json!({ "triggers": [
            { "enabled": false, "kind": "Drag", "modifiers": "Shift" },
            { "kind": "KeyChord", "modifiers": "Ctrl, Alt" },
            { "enabled": true, "kind": "Drag", "modifiers": "Alt, Ctrl, Meta" }
        ]});
        assert_eq!(gesture_text(&settings), "Ctrl + Alt + Win");
        assert_eq!(gesture_text(&json!({ "triggers": [{ "kind": "Drag", "modifiers": "Ctrl" }] })), "Ctrl");
        assert_eq!(gesture_text(&json!({ "triggers": [{ "kind": "Drag", "modifiers": "None" }] })), "—");
        assert_eq!(gesture_text(&Value::Null), "—");
    }

    #[test]
    fn tooltip_switches_with_the_paused_state() {
        i18n::init("zh-TW");
        let settings = json!({ "triggers": [{ "kind": "Drag", "modifiers": "Ctrl" }] });
        assert_eq!(tooltip(false, &settings), "Filee：按住 Ctrl 拖曳檔案");
        assert_eq!(tooltip(true, &settings), "快捷操作已暫停");
    }
}
