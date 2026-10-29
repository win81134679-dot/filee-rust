//! One Filee Rust per user session: a named mutex; later launches send their arguments to the running one's
//! main window with `WM_COPYDATA` (UTF-16, NUL-separated) and exit.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_COPYDATA};

/// Marks our WM_COPYDATA ("FLit").
const MAGIC: usize = 0x464C_6974;

pub struct Instance {
    pub is_first: bool,
    _mutex: HANDLE,
}

pub fn acquire() -> Instance {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let name = HSTRING::from(format!("Local\\FileeLite-{user}"));
    unsafe {
        let mutex = CreateMutexW(None, true, &name).unwrap_or_default();
        let is_first = GetLastError() != ERROR_ALREADY_EXISTS;
        Instance { is_first, _mutex: mutex }
    }
}

/// Sends `args` to the running Filee Rust. False when it isn't there or doesn't answer within 5 s.
pub fn forward(args: &[String]) -> bool {
    let mut data: Vec<u16> = Vec::new();
    for a in args {
        data.extend(a.encode_utf16());
        data.push(0);
    }
    unsafe {
        for _ in 0..20 {
            if let Ok(hwnd) = FindWindowW(crate::MAIN_CLASS, PCWSTR::null()) {
                let cds = COPYDATASTRUCT {
                    dwData: MAGIC,
                    cbData: (data.len() * 2) as u32,
                    lpData: data.as_ptr() as *mut _,
                };
                let mut result = 0usize;
                let ok = SendMessageTimeoutW(
                    hwnd,
                    WM_COPYDATA,
                    WPARAM(0),
                    LPARAM(&cds as *const _ as isize),
                    SMTO_ABORTIFHUNG,
                    5000,
                    Some(&mut result),
                );
                return ok.0 != 0 && result == 1;
            }
            // The first instance may still be creating its window.
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    false
}

/// The arguments in a WM_COPYDATA from [`forward`], or None for anything else.
pub fn received(lparam: LPARAM) -> Option<Vec<String>> {
    unsafe {
        let cds = (lparam.0 as *const COPYDATASTRUCT).as_ref()?;
        if cds.dwData != MAGIC || cds.lpData.is_null() || cds.cbData % 2 != 0 {
            return None;
        }
        let words = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
        Some(
            words
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(String::from_utf16_lossy)
                .collect(),
        )
    }
}
