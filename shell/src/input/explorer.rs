//! Click mode: the files selected in the foreground File Explorer window (its active tab on Windows 11) or on the
//! desktop (spec §3.2, upstream `WindowsPlatformServices.SelectionOf`, `ExplorerTabs`, `DesktopShellView`).
//!
//! Read-only COM: `IShellWindows` → each window's `IServiceProvider` → `IShellBrowser` → `IShellView` →
//! `IFolderView2::GetSelection`. Must run on an STA thread (the UI thread). Only existing files come back; folders
//! are dropped like upstream does for click mode.

use std::path::PathBuf;

use windows::core::{w, Interface};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_ALL};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    IFolderView2, IShellBrowser, IShellItemArray, IShellView, IShellWindows, ShellWindows, SIGDN_FILESYSPATH,
    SID_STopLevelBrowser, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetAncestor, IsWindowVisible, GA_ROOT};

use super::scope;

/// One window of `IShellWindows` (one per tab on Windows 11): its top-level window and its tab window (0 = unknown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub top: isize,
    pub tab: isize,
}

/// A `ShellTabWindowClass` child of an Explorer window, in z-order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabWindow {
    pub handle: isize,
    pub visible: bool,
}

/// The active tab among one Explorer window's tab windows: the visible one, else the first in z-order; 0 when none.
pub fn pick_active_tab(tabs: &[TabWindow]) -> isize {
    if let Some(t) = tabs.iter().find(|t| t.visible) {
        return t.handle;
    }
    tabs.first().map(|t| t.handle).unwrap_or(0)
}

/// Indexes of the entries to read the selection from, in order. Only entries of the foreground window count; when one
/// of them is the active tab it is the only one (another tab's selection, even a stale one, is never what the user
/// means). Without tabs, or when no entry gives out its tab window, every entry of the window is tried in order.
pub fn candidates(entries: &[Entry], foreground: isize, active_tab: isize) -> Vec<usize> {
    let mut same_window = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if e.top != foreground {
            continue;
        }
        if active_tab != 0 && e.tab == active_tab {
            return vec![i];
        }
        same_window.push(i);
    }
    same_window
}

fn tab_windows_of(top: HWND) -> Vec<TabWindow> {
    let mut out = Vec::new();
    let mut child: Option<HWND> = None;
    unsafe {
        while let Ok(next) = FindWindowExW(Some(top), child, w!("ShellTabWindowClass"), None) {
            if next.0.is_null() {
                break;
            }
            out.push(TabWindow { handle: next.0 as isize, visible: IsWindowVisible(next).as_bool() });
            child = Some(next);
        }
    }
    out
}

/// The selected files of the foreground Explorer window or the desktop; empty when there is none.
/// Call on the UI (STA) thread.
pub fn selection() -> Vec<PathBuf> {
    let Some(root) = scope::foreground_root() else { return Vec::new() };
    let on_desktop = scope::class_name_of(root).as_deref().is_some_and(scope::is_desktop_class);
    let result = if on_desktop { desktop_selection() } else { window_selection(root) };
    match result {
        Ok(files) => files,
        Err(e) => {
            crate::warn!("Reading the file manager selection failed: {e}");
            Vec::new()
        }
    }
}

fn window_selection(foreground: HWND) -> windows::core::Result<Vec<PathBuf>> {
    unsafe {
        let shell: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let count = shell.Count()?;
        let mut entries = Vec::new();
        let mut browsers = Vec::new();
        for i in 0..count {
            // IE windows and anything that isn't a shell browser fail here; skip them like upstream does.
            let Ok(item) = shell.Item(&VARIANT::from(i)) else { continue };
            let Ok(provider) = item.cast::<IServiceProvider>() else { continue };
            let Ok(browser) = provider.QueryService::<IShellBrowser>(&SID_STopLevelBrowser) else { continue };
            let tab = browser.GetWindow().map(|h| h.0 as isize).unwrap_or(0);
            let top = if tab == 0 {
                0
            } else {
                let root = GetAncestor(HWND(tab as *mut _), GA_ROOT);
                if root.0.is_null() { tab } else { root.0 as isize }
            };
            entries.push(Entry { top, tab });
            browsers.push(browser);
        }
        let active_tab = pick_active_tab(&tab_windows_of(foreground));
        for i in candidates(&entries, foreground.0 as isize, active_tab) {
            let files = selected_files(&browsers[i]);
            if !files.is_empty() {
                return Ok(files);
            }
        }
        Ok(Vec::new())
    }
}

fn desktop_selection() -> windows::core::Result<Vec<PathBuf>> {
    unsafe {
        let shell: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let mut hwnd = 0i32;
        let desktop = shell.FindWindowSW(&VARIANT::from(0i32), &VARIANT::default(), SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let provider: IServiceProvider = desktop.cast()?;
        let browser = provider.QueryService::<IShellBrowser>(&SID_STopLevelBrowser)?;
        Ok(selected_files(&browser))
    }
}

/// The files (not folders) selected in the browser's active view.
fn selected_files(browser: &IShellBrowser) -> Vec<PathBuf> {
    unsafe {
        let Ok(view) = browser.QueryActiveShellView() else { return Vec::new() };
        files_of_view(&view).unwrap_or_default()
    }
}

unsafe fn files_of_view(view: &IShellView) -> windows::core::Result<Vec<PathBuf>> {
    let folder_view: IFolderView2 = view.cast()?;
    let items: IShellItemArray = folder_view.GetSelection(false)?;
    let count = items.GetCount()?;
    let mut files = Vec::new();
    for i in 0..count {
        let Ok(item) = items.GetItemAt(i) else { continue };
        let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else { continue };
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as *const _));
        if let Ok(path) = path {
            let path = PathBuf::from(path);
            if path.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(top: isize, tab: isize) -> Entry {
        Entry { top, tab }
    }

    #[test]
    fn active_tab_is_the_visible_one_else_the_first() {
        let tabs = [TabWindow { handle: 1, visible: false }, TabWindow { handle: 2, visible: true }, TabWindow { handle: 3, visible: false }];
        assert_eq!(pick_active_tab(&tabs), 2);
        let hidden = [TabWindow { handle: 7, visible: false }, TabWindow { handle: 8, visible: false }];
        assert_eq!(pick_active_tab(&hidden), 7);
        assert_eq!(pick_active_tab(&[]), 0);
    }

    #[test]
    fn candidates_only_use_the_active_tab_when_it_is_known() {
        let entries = [e(100, 11), e(100, 12), e(200, 21), e(100, 13)];
        assert_eq!(candidates(&entries, 100, 12), vec![1]);
        assert_eq!(candidates(&entries, 100, 13), vec![3]);
    }

    #[test]
    fn candidates_fall_back_to_every_entry_of_the_window() {
        let entries = [e(100, 11), e(100, 12), e(200, 21)];
        // no tab windows at all (Windows 10 / desktop)
        assert_eq!(candidates(&entries, 100, 0), vec![0, 1]);
        // active tab matches none of the entries
        assert_eq!(candidates(&entries, 100, 99), vec![0, 1]);
        // other window
        assert_eq!(candidates(&entries, 300, 0), Vec::<usize>::new());
        assert_eq!(candidates(&entries, 200, 0), vec![2]);
    }

    /// Read-only COM query of whatever Explorer window is in front when the test runs. Run it by hand:
    /// `cargo test -- --ignored --nocapture reads_the_foreground_selection` while a File Explorer window with a
    /// selected file is in front (start it from a terminal and switch to Explorer within a few seconds).
    #[test]
    #[ignore]
    fn reads_the_foreground_selection() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
        let files = selection();
        println!("selected files: {files:?}");
    }
}
