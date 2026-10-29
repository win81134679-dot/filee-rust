//! OLE drop target of the donut window and the reading of what is dragged (spec 01 §2.2, §2.4, §7.1).
//!
//! CF_HDROP (local files, also Firefox's temporary copy of a dragged picture) is read first. Without it, a
//! browser's virtual files (Chrome / Edge: `FileGroupDescriptorW` + `FileContents`) are saved into
//! `%LOCALAPPDATA%\FileeLite\web\<drop>\<n>\` on the first DragEnter, so the donut picks its face by their
//! extension, and the same copies are used for the drop (upstream's `BrowserDrags` / `WebDrop`, without its
//! deferred download). Links without bytes (`UniformResourceLocatorW`, `DownloadURL`) are still not fetched.

use std::path::PathBuf;

use windows::core::{implement, w, Ref};
use windows::Win32::Foundation::POINTL;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, ReleaseStgMedium, CF_HDROP, DROPEFFECT,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

use super::window;

#[implement(IDropTarget)]
pub struct DropTarget;

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Ref<'_, IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let (files, offered) = match pdataobj.as_ref() {
            Some(data) => {
                let mut files = read_files(data);
                if files.is_empty() {
                    files = virtual_files_cached(data);
                }
                (files, offers_files(data))
            }
            None => (Vec::new(), false),
        };
        unsafe {
            *pdweffect = window::drag_enter(files, offered, (pt.x, pt.y), *pdweffect);
        }
        Ok(())
    }

    fn DragOver(&self, _grfkeystate: MODIFIERKEYS_FLAGS, pt: &POINTL, pdweffect: *mut DROPEFFECT) -> windows::core::Result<()> {
        unsafe {
            *pdweffect = window::drag_over((pt.x, pt.y), *pdweffect);
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        window::drag_leave();
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Ref<'_, IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // Read again now: this is where a source that hands over its bytes late would be asked for them.
        let files = match pdataobj.as_ref() {
            Some(data) => {
                let files = read_files(data);
                if files.is_empty() { virtual_files_cached(data) } else { files }
            }
            None => Vec::new(),
        };
        unsafe {
            *pdweffect = window::drop_files(files, (pt.x, pt.y), *pdweffect);
        }
        Ok(())
    }
}

fn hdrop_format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn named_format(cf: u32) -> FORMATETC {
    FORMATETC { cfFormat: cf as u16, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: u32::MAX }
}

/// The local files of a drag (CF_HDROP): existing, not a drive root, no duplicates.
pub fn read_files(data: &IDataObject) -> Vec<PathBuf> {
    let fmt = hdrop_format();
    unsafe {
        if data.QueryGetData(&fmt).is_err() {
            return Vec::new();
        }
        let Ok(mut medium) = data.GetData(&fmt) else { return Vec::new() };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut raw = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            raw.push(PathBuf::from(String::from_utf16_lossy(&buf[..got.min(len)])));
        }
        ReleaseStgMedium(&mut medium);
        clean(raw)
    }
}

/// Upstream's `DraggedFiles.Local`: trailing separators trimmed, must exist, not a drive root, case-insensitive dedupe.
fn clean(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let s = p.to_string_lossy();
        let trimmed = s.trim_end_matches(['\\', '/']);
        if trimmed.is_empty() || is_drive_root(trimmed) {
            continue;
        }
        let path = PathBuf::from(trimmed);
        if path.exists() && seen.insert(trimmed.to_lowercase()) {
            out.push(path);
        }
    }
    out
}

fn is_drive_root(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

/// A source that may carry files we cannot read yet (a browser's virtual files or links): counts as "offered".
fn offers_files(data: &IDataObject) -> bool {
    if has(data, hdrop_format()) {
        return true;
    }
    unsafe {
        [
            RegisterClipboardFormatW(w!("FileGroupDescriptorW")),
            RegisterClipboardFormatW(w!("UniformResourceLocatorW")),
            RegisterClipboardFormatW(w!("UniformResourceLocator")),
            RegisterClipboardFormatW(w!("text/x-moz-url")),
            RegisterClipboardFormatW(w!("DownloadURL")),
        ]
        .into_iter()
        .any(|cf| cf != 0 && has(data, named_format(cf)))
    }
}

fn has(data: &IDataObject, fmt: FORMATETC) -> bool {
    unsafe { data.QueryGetData(&fmt).is_ok() }
}

// ---- a browser's virtual files ---------------------------------------------------------------------------

/// Upstream refuses these by extension: nothing from a browser is ever a program, script or shortcut.
const REFUSED: [&str; 24] = [
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi", "msp", "scr", "pif",
    "cpl", "hta", "lnk", "url", "reg", "dll", "jar", "appx",
];
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_NAME: usize = 120;

thread_local! {
    /// The copies saved for the drag in progress: (data object identity, files). DragEnter fills it; the Drop of
    /// the same drag reuses it instead of asking the browser for the bytes again.
    static LAST_VIRTUAL: std::cell::RefCell<Option<(usize, Vec<PathBuf>)>> = const { std::cell::RefCell::new(None) };
}

fn virtual_files_cached(data: &IDataObject) -> Vec<PathBuf> {
    let key = windows::core::Interface::as_raw(data) as usize;
    if let Some(files) = LAST_VIRTUAL.with(|c| c.borrow().as_ref().filter(|(k, _)| *k == key).map(|(_, f)| f.clone())) {
        if files.iter().all(|f| f.exists()) {
            return files;
        }
    }
    let files = read_virtual_files(data);
    LAST_VIRTUAL.with(|c| *c.borrow_mut() = Some((key, files.clone())));
    files
}

/// Saves the virtual files of `data` (FileGroupDescriptorW + FileContents) and returns their paths.
fn read_virtual_files(data: &IDataObject) -> Vec<PathBuf> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::Com::{TYMED_ISTREAM};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::UI::Shell::{FILEDESCRIPTORW, FILEGROUPDESCRIPTORW};

    let (cf_group, cf_contents) =
        unsafe { (RegisterClipboardFormatW(w!("FileGroupDescriptorW")), RegisterClipboardFormatW(w!("FileContents"))) };
    if cf_group == 0 || cf_contents == 0 {
        return Vec::new();
    }
    let mut group_fmt = named_format(cf_group);
    group_fmt.tymed = TYMED_HGLOBAL.0 as u32;
    let names: Vec<(String, u64)> = unsafe {
        let Ok(mut medium) = data.GetData(&group_fmt) else { return Vec::new() };
        let h: HGLOBAL = medium.u.hGlobal;
        let base = GlobalLock(h) as *const u8;
        let mut out = Vec::new();
        if !base.is_null() && GlobalSize(h) >= std::mem::size_of::<u32>() {
            let group = &*(base as *const FILEGROUPDESCRIPTORW);
            let first = group.fgd.as_ptr() as *const FILEDESCRIPTORW;
            let room = (GlobalSize(h) - std::mem::size_of::<u32>()) / std::mem::size_of::<FILEDESCRIPTORW>();
            for i in 0..(group.cItems as usize).min(room).min(64) {
                // The descriptor is packed (1-byte aligned): copy it out before touching its fields.
                let d: FILEDESCRIPTORW = std::ptr::read_unaligned(first.add(i));
                let file_name = d.cFileName;
                let len = file_name.iter().position(|&c| c == 0).unwrap_or(file_name.len());
                let size = ((d.nFileSizeHigh as u64) << 32) | d.nFileSizeLow as u64;
                out.push((String::from_utf16_lossy(&file_name[..len]), size));
            }
        }
        let _ = GlobalUnlock(h);
        ReleaseStgMedium(&mut medium);
        out
    };
    if names.is_empty() {
        return Vec::new();
    }

    let drop_dir = web_root().join(format!("{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
    let mut saved = Vec::new();
    for (index, (name, size)) in names.iter().enumerate() {
        let Some(name) = safe_name(name) else {
            crate::warn!("Browser drop: refused or unusable name {name:?}");
            continue;
        };
        if *size > MAX_BYTES {
            crate::warn!("Browser drop: {name} is over 2 GB");
            continue;
        }
        let mut fmt = named_format(cf_contents);
        fmt.lindex = index as i32;
        fmt.tymed = (TYMED_HGLOBAL.0 | TYMED_ISTREAM.0) as u32;
        let bytes: Option<Vec<u8>> = unsafe {
            match data.GetData(&fmt) {
                Ok(mut medium) => {
                    let out = if medium.tymed == TYMED_HGLOBAL.0 as u32 {
                        let h = medium.u.hGlobal;
                        let p = GlobalLock(h) as *const u8;
                        let n = GlobalSize(h);
                        // An HGLOBAL may be rounded up: the descriptor's size, when it has one, is the real length.
                        let n = if *size > 0 { (*size as usize).min(n) } else { n };
                        let v = if p.is_null() { None } else { Some(std::slice::from_raw_parts(p, n).to_vec()) };
                        let _ = GlobalUnlock(h);
                        v
                    } else if medium.tymed == TYMED_ISTREAM.0 as u32 {
                        (*medium.u.pstm).as_ref().map(|stream| {
                            let mut v = Vec::new();
                            let mut buf = vec![0u8; 1 << 16];
                            loop {
                                let mut read = 0u32;
                                let hr = stream.Read(buf.as_mut_ptr() as *mut _, buf.len() as u32, Some(&mut read));
                                if hr.is_err() || read == 0 || v.len() as u64 > MAX_BYTES {
                                    break;
                                }
                                v.extend_from_slice(&buf[..read as usize]);
                            }
                            v
                        })
                    } else {
                        None
                    };
                    ReleaseStgMedium(&mut medium);
                    out
                }
                Err(_) => None,
            }
        };
        let Some(bytes) = bytes.filter(|b| !b.is_empty()) else {
            crate::warn!("Browser drop: no bytes for {name}");
            continue;
        };
        let dir = drop_dir.join(index.to_string());
        let path = dir.join(&name);
        if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &bytes)).is_ok() {
            saved.push(path);
        }
    }
    if !saved.is_empty() {
        crate::info!("Browser drop: saved {} file(s) under {}", saved.len(), drop_dir.display());
    }
    saved
}

/// `%LOCALAPPDATA%\FileeLite\web`; folders older than a day are removed on the way (a drop's copies are only needed
/// until its conversion has read them).
fn web_root() -> PathBuf {
    let root = crate::paths::lite_dir().join("web");
    if let Ok(entries) = std::fs::read_dir(&root) {
        let day = std::time::Duration::from_secs(24 * 3600);
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > day);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    root
}

/// A plain file name for a browser's file (upstream `WebFileNames`): no folders, no invalid characters, no device
/// names, at most 120 characters, never a program or script. None when nothing usable is left.
fn safe_name(raw: &str) -> Option<String> {
    let base = raw.rsplit(['\\', '/']).next().unwrap_or("").trim().trim_end_matches(['.', ' ']);
    let mut name: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    if name.is_empty() {
        return None;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i + 1..].to_ascii_lowercase()),
        _ => (name.clone(), String::new()),
    };
    if REFUSED.contains(&ext.as_str()) {
        return None;
    }
    let device = ["con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "lpt1", "lpt2", "lpt3"];
    if device.contains(&stem.to_ascii_lowercase().as_str()) {
        name = format!("_{name}");
    }
    if name.chars().count() > MAX_NAME {
        let keep_ext = if ext.is_empty() { String::new() } else { format!(".{ext}") };
        let room = MAX_NAME.saturating_sub(keep_ext.chars().count());
        name = stem.chars().take(room).collect::<String>() + &keep_ext;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_keeps_existing_files_and_folders_once_and_drops_drive_roots() {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().unwrap().to_path_buf();
        let upper = PathBuf::from(exe.to_string_lossy().to_uppercase());
        let with_sep = PathBuf::from(format!("{}\\", dir.display()));
        let out = clean(vec![
            exe.clone(),
            upper,
            PathBuf::from(r"C:\"),
            PathBuf::from("C:"),
            PathBuf::from(r"C:\definitely\not\here.png"),
            with_sep,
            PathBuf::new(),
        ]);
        assert_eq!(out, vec![exe, dir], "the exe once (case-insensitive), the folder without its trailing slash");
    }

    #[test]
    fn browser_file_names_are_made_safe() {
        assert_eq!(safe_name("photo.webp").as_deref(), Some("photo.webp"));
        assert_eq!(safe_name(r"C:\x\..\evil\pic.png").as_deref(), Some("pic.png"));
        assert_eq!(safe_name("a<b>:c?.jpg").as_deref(), Some("a_b__c_.jpg"));
        assert_eq!(safe_name("con.png").as_deref(), Some("_con.png"));
        assert_eq!(safe_name("setup.EXE"), None);
        assert_eq!(safe_name("run.ps1"), None);
        assert_eq!(safe_name("link.url"), None);
        assert_eq!(safe_name("   "), None);
        let long = format!("{}.jpeg", "x".repeat(300));
        let cut = safe_name(&long).unwrap();
        assert_eq!(cut.chars().count(), 120);
        assert!(cut.ends_with(".jpeg"));
    }

    #[test]
    fn drive_roots_are_recognised() {
        assert!(is_drive_root("C:"));
        assert!(is_drive_root("z:"));
        assert!(!is_drive_root(r"C:\x"));
        assert!(!is_drive_root("1:"));
    }
}
