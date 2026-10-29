//! The Win32 side: `WH_MOUSE_LL` and `WH_KEYBOARD_LL` on a thread of their own with its own message loop, and a
//! worker thread that does everything that takes time (WindowFromPoint, process names, scope, the gesture engine).
//!
//! The hook callbacks only record: they update the modifier / button / cursor state, push one small message on a
//! channel and call `CallNextHookEx` at once. Nothing is ever swallowed. Windows removes a low-level hook whose
//! callback is slow (`LowLevelHooksTimeout`), which is why scope checks live on the worker.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, GetSystemMetrics, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, SM_CXDRAG, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use super::engine::{Config, Engine, Raw};
use super::keys::{self, Button};
use super::scope::ScopeChecker;
use crate::events::{self, AppEvent};

// ---- shared state ----

enum Msg {
    Raw(Raw),
    Config,
    Stop,
}

struct Running {
    tx: Sender<Msg>,
    hook_thread_id: u32,
    hook: JoinHandle<()>,
    worker: JoinHandle<()>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
/// What the hook callbacks send to; `None` when stopped.
static TX: Mutex<Option<Sender<Msg>>> = Mutex::new(None);
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);
static WATCH_FLIP: AtomicBool = AtomicBool::new(false);
/// Modifier keys held, left and right separately (see `MOD_KEYS`).
static MOD_BITS: AtomicU16 = AtomicU16::new(0);
static BUTTONS: AtomicU8 = AtomicU8::new(0);
/// Last cursor position, `(x as u32) << 32 | (y as u32)`; `UNSET_CURSOR` before the first mouse event.
static CURSOR: AtomicU64 = AtomicU64::new(UNSET_CURSOR);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
const UNSET_CURSOR: u64 = (i32::MIN as u32 as u64) << 32 | (i32::MIN as u32 as u64);

/// Virtual-key codes of the modifier keys and their bit in `MOD_BITS`.
const MOD_KEYS: [(u32, u16); 8] = [
    (0xA0, 1 << 0), // left shift
    (0xA1, 1 << 1),
    (0xA2, 1 << 2), // left ctrl
    (0xA3, 1 << 3),
    (0xA4, 1 << 4), // left alt
    (0xA5, 1 << 5),
    (0x5B, 1 << 6), // left win
    (0x5C, 1 << 7),
];

/// `MOD_BITS` folded into `keys::CTRL | ALT | SHIFT | META` (left and right alike).
pub fn mods_from_bits(bits: u16) -> u8 {
    let mut m = 0;
    if bits & 0b0000_0011 != 0 {
        m |= keys::SHIFT;
    }
    if bits & 0b0000_1100 != 0 {
        m |= keys::CTRL;
    }
    if bits & 0b0011_0000 != 0 {
        m |= keys::ALT;
    }
    if bits & 0b1100_0000 != 0 {
        m |= keys::META;
    }
    m
}

fn mod_bit(vk: u32) -> Option<u16> {
    MOD_KEYS.iter().find(|(k, _)| *k == vk).map(|(_, b)| *b)
}

fn pack(x: i32, y: i32) -> u64 {
    ((x as u32 as u64) << 32) | (y as u32 as u64)
}

fn unpack(v: u64) -> (i32, i32) {
    ((v >> 32) as u32 as i32, v as u32 as i32)
}

fn send(raw: Raw) {
    if let Ok(guard) = TX.lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(Msg::Raw(raw));
        }
    }
}

// ---- hook callbacks (the OS calls these on the hook thread; keep them tiny) ----

/// Rebuilds the modifier state from the keyboard; called on a mouse press and on every non-modifier key-down so a
/// missed key-up (Win+L, a secure desktop, an elevated window) can't leave a modifier stuck.
fn resync_modifiers() {
    let mut bits = 0u16;
    for (vk, bit) in MOD_KEYS {
        if key_is_down(vk) {
            bits |= bit;
        }
    }
    MOD_BITS.store(bits, Ordering::Relaxed);
}

fn key_is_down(vk: u32) -> bool {
    (unsafe { GetAsyncKeyState(vk as i32) }) as u16 & 0x8000 != 0
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        let _ = catch_unwind(AssertUnwindSafe(|| on_mouse(wparam.0 as u32, &*(lparam.0 as *const MSLLHOOKSTRUCT))));
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn on_mouse(message: u32, info: &MSLLHOOKSTRUCT) {
    let (x, y) = (info.pt.x, info.pt.y);
    let button = match message {
        WM_LBUTTONDOWN | WM_LBUTTONUP => Some(Button::Left),
        WM_RBUTTONDOWN | WM_RBUTTONUP => Some(Button::Right),
        WM_MBUTTONDOWN | WM_MBUTTONUP => Some(Button::Middle),
        WM_XBUTTONDOWN | WM_XBUTTONUP => match (info.mouseData >> 16) & 0xFFFF {
            1 => Some(Button::X1),
            2 => Some(Button::X2),
            _ => None,
        },
        _ => None,
    };
    match message {
        WM_MOUSEMOVE => {
            CURSOR.store(pack(x, y), Ordering::Relaxed);
            let mods = mods_from_bits(MOD_BITS.load(Ordering::Relaxed));
            let held = Button::held(BUTTONS.load(Ordering::Relaxed));
            send(Raw::Move { x, y, mods, held });
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => {
            if let Some(button) = button {
                CURSOR.store(pack(x, y), Ordering::Relaxed);
                resync_modifiers();
                BUTTONS.fetch_or(button.bit(), Ordering::Relaxed);
                let mods = mods_from_bits(MOD_BITS.load(Ordering::Relaxed));
                send(Raw::Down { button, x, y, mods });
            }
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP => {
            if let Some(button) = button {
                CURSOR.store(pack(x, y), Ordering::Relaxed);
                BUTTONS.fetch_and(!button.bit(), Ordering::Relaxed);
                send(Raw::Up { x, y });
            }
        }
        WM_MOUSEWHEEL => {
            if WATCH_FLIP.load(Ordering::Relaxed) && (info.mouseData >> 16) as u16 as i16 != 0 {
                send(Raw::Wheel);
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        let _ = catch_unwind(AssertUnwindSafe(|| on_key(wparam.0 as u32, &*(lparam.0 as *const KBDLLHOOKSTRUCT))));
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn on_key(message: u32, info: &KBDLLHOOKSTRUCT) {
    let down = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return,
    };
    let vk = info.vkCode;
    if let Some(bit) = mod_bit(vk) {
        if down {
            MOD_BITS.fetch_or(bit, Ordering::Relaxed);
        } else {
            MOD_BITS.fetch_and(!bit, Ordering::Relaxed);
        }
    }
    if down && mod_bit(vk).is_none() {
        resync_modifiers();
    }
    let mods = mods_from_bits(MOD_BITS.load(Ordering::Relaxed));
    send(Raw::Key { vk, down, mods });
}

// ---- threads ----

fn hook_thread(ready: Sender<Result<u32, String>>) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        // Make sure the thread has a message queue before anyone posts WM_QUIT to it.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let module = GetModuleHandleW(None).unwrap_or_default();
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0);
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0);
        let (mouse, keyboard) = match (mouse, keyboard) {
            (Ok(m), Ok(k)) => (m, k),
            (m, k) => {
                let why = format!("mouse hook: {:?}, keyboard hook: {:?}", m.as_ref().err(), k.as_ref().err());
                if let Ok(m) = m {
                    let _ = UnhookWindowsHookEx(m);
                }
                if let Ok(k) = k {
                    let _ = UnhookWindowsHookEx(k);
                }
                let _ = ready.send(Err(why));
                return;
            }
        };
        let _ = ready.send(Ok(GetCurrentThreadId()));
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        let _ = UnhookWindowsHookEx(mouse);
        let _ = UnhookWindowsHookEx(keyboard);
    }
}

fn worker_thread(rx: Receiver<Msg>) {
    let threshold = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(2);
    let epoch = Instant::now();
    let mut engine = Engine::new(threshold, epoch);
    let mut checker = ScopeChecker::new();
    let apply = |engine: &mut Engine, checker: &mut ScopeChecker| {
        let cfg = CONFIG.lock().ok().and_then(|c| c.clone()).unwrap_or_default();
        checker.excluded = super::scope::normalize_excluded(&cfg.excluded);
        engine.configure(&cfg);
    };
    apply(&mut engine, &mut checker);
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_ok() {
        engine.set_last_position(point.x, point.y);
    }

    loop {
        let msg = match engine.next_deadline() {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            },
        };
        // A panic in one message (a Win32 lookup, the engine) must not end the thread: that would silently kill every
        // gesture until restart. Log it and carry on with the next message.
        let processed = catch_unwind(AssertUnwindSafe(|| match msg {
            None => Some(engine.tick(Instant::now())),
            Some(Msg::Stop) => None,
            Some(Msg::Config) => {
                apply(&mut engine, &mut checker);
                Some(Vec::new())
            }
            Some(Msg::Raw(raw)) => {
                if let Raw::Key { down: true, .. } = raw {
                    // a key-up we never saw would make this press look like an auto-repeat
                    engine.prune_down_keys(key_is_down);
                }
                let watch = WATCH_FLIP.load(Ordering::Relaxed);
                let mut scope = |t: &super::gesture::Trigger, x: i32, y: i32| checker.in_scope(t, x, y);
                Some(engine.handle(raw, Instant::now(), watch, &mut scope))
            }
        }));
        let events = match processed {
            Ok(Some(events)) => events,
            Ok(None) => break,
            Err(payload) => {
                let what = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                crate::warn!("The input worker recovered from a panic: {what}");
                Vec::new()
            }
        };
        for event in events {
            events::post(AppEvent::Input(event));
        }
    }
}

// ---- public face (re-exported by mod.rs) ----

pub fn start() {
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if running.is_some() {
        return;
    }
    MOD_BITS.store(0, Ordering::Relaxed);
    BUTTONS.store(0, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Msg>();
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
    let worker = match std::thread::Builder::new().name("filee input worker".into()).spawn(move || worker_thread(rx)) {
        Ok(h) => h,
        Err(e) => {
            crate::warn!("Could not start the input worker: {e}");
            *TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return;
        }
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let hook = match std::thread::Builder::new().name("filee input hooks".into()).spawn(move || hook_thread(ready_tx)) {
        Ok(h) => h,
        Err(e) => {
            crate::warn!("Could not start the hook thread: {e}");
            abort_start(tx, worker);
            return;
        }
    };
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(thread_id)) => {
            HOOK_THREAD_ID.store(thread_id, Ordering::SeqCst);
            *running = Some(Running { tx, hook_thread_id: thread_id, hook, worker });
            crate::info!("Global mouse and keyboard hooks installed");
        }
        Ok(Err(why)) => {
            crate::warn!("Installing the global hooks failed ({why}); gestures will not work");
            let _ = hook.join();
            abort_start(tx, worker);
        }
        Err(_) => {
            crate::warn!("Installing the global hooks timed out; gestures will not work");
            abort_start(tx, worker);
        }
    }
}

fn abort_start(tx: Sender<Msg>, worker: JoinHandle<()>) {
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let _ = tx.send(Msg::Stop);
    let _ = worker.join();
}

pub fn stop() {
    let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(running) = running else { return };
    // No more messages from the callbacks, then unhook (WM_QUIT ends the hook thread's loop), then end the worker.
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    unsafe {
        let _ = PostThreadMessageW(running.hook_thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
    }
    let _ = running.hook.join();
    let _ = running.tx.send(Msg::Stop);
    let _ = running.worker.join();
    HOOK_THREAD_ID.store(0, Ordering::SeqCst);
    WATCH_FLIP.store(false, Ordering::Relaxed);
    crate::info!("Global hooks removed");
}

pub fn apply(settings: &serde_json::Value) {
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(Config::from_settings(settings));
    if let Some(running) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = running.tx.send(Msg::Config);
    }
}

pub fn set_watch_flip(on: bool) {
    WATCH_FLIP.store(on, Ordering::Relaxed);
}

pub fn last_cursor() -> (i32, i32) {
    let v = CURSOR.load(Ordering::Relaxed);
    if v != UNSET_CURSOR {
        return unpack(v);
    }
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_ok() {
        (p.x, p.y)
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bits_fold_left_and_right() {
        assert_eq!(mods_from_bits(0), 0);
        assert_eq!(mods_from_bits(1 << 2), keys::CTRL);
        assert_eq!(mods_from_bits(1 << 3), keys::CTRL);
        assert_eq!(mods_from_bits((1 << 2) | (1 << 3)), keys::CTRL);
        assert_eq!(mods_from_bits((1 << 2) | (1 << 5)), keys::CTRL | keys::ALT);
        assert_eq!(mods_from_bits(1 << 1), keys::SHIFT);
        assert_eq!(mods_from_bits(1 << 7), keys::META);
        assert_eq!(mods_from_bits(0xFF), keys::CTRL | keys::ALT | keys::SHIFT | keys::META);
    }

    #[test]
    fn modifier_virtual_keys_map_to_distinct_bits() {
        let mut seen = 0u16;
        for (vk, bit) in MOD_KEYS {
            assert_eq!(mod_bit(vk), Some(bit));
            assert_eq!(seen & bit, 0);
            seen |= bit;
        }
        assert_eq!(seen, 0xFF);
        assert_eq!(mod_bit(0x41), None);
    }

    #[test]
    fn cursor_packing_round_trips_negative_coordinates() {
        for (x, y) in [(0, 0), (1920, 1080), (-1, -1), (-1920, 300), (i32::MAX, i32::MIN + 1)] {
            assert_eq!(unpack(pack(x, y)), (x, y));
            assert_ne!(pack(x, y), UNSET_CURSOR);
        }
    }
}
