//! Cross-thread event bus: any thread `post`s an [`AppEvent`]; the UI thread's message window gets
//! `WM_APP_EVENT` and drains the queue with [`drain`]. Keeps hooks and workers from touching windows directly.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::model::{DonutEvent, InputEvent, JobStatus};

/// Posted to the main message window when the queue has something.
pub const WM_APP_EVENT: u32 = WM_APP + 1;

#[derive(Debug)]
pub enum AppEvent {
    Input(InputEvent),
    Donut(DonutEvent),
    /// A job's status changed (started, progressed, finished).
    Job(JobStatus),
    /// Files handed over by a later launch (`filee-lite.exe --convert …`, the Explorer verb).
    Convert(Vec<std::path::PathBuf>),
    /// A file under %APPDATA%\Filee changed: reload settings / presets / profiles.
    DataChanged,
    /// Show upstream's settings window.
    OpenSettings,
    Quit,
}

static QUEUE: Mutex<VecDeque<AppEvent>> = Mutex::new(VecDeque::new());
static TARGET: AtomicIsize = AtomicIsize::new(0);

/// The window that receives `WM_APP_EVENT` (set once by main).
pub fn set_target(hwnd: HWND) {
    TARGET.store(hwnd.0 as isize, Ordering::SeqCst);
}

pub fn post(event: AppEvent) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push_back(event);
    let target = TARGET.load(Ordering::SeqCst);
    if target != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_EVENT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Everything queued so far, oldest first.
pub fn drain() -> Vec<AppEvent> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
}
