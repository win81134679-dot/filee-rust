//! The donut's window: a layered, topmost, non-activating popup whose pixels come from `render` and whose state is a
//! `logic::Menu`. Also the controller (spec 01 §2.2, §3.3): when it shows, how a drag or a click ends it, timers.
//!
//! Everything runs on the UI thread; the state lives in a thread-local that window messages and OLE callbacks reach
//! through [`with`]. A callback that arrives while the state is already borrowed (a Win32 call we made sent a
//! message to our own window) is dropped: those are echoes of what we are doing, never input.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Ole::{RegisterDragDrop, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::geometry::*;
use super::logic::{Effect, Key, Menu, Mode, Style};
use super::render::{Renderer, Surface};
use super::FacesProvider;
use crate::events::{self, AppEvent};
use crate::model::{DonutEvent, Faces, Palette};

const CLASS: windows::core::PCWSTR = w!("FileeLiteDonut");
/// Sent when the pointer leaves the window (asked for with TrackMouseEvent).
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_FRAME: usize = 1;
const TIMER_EMPTY_DRAG: usize = 2;
const TIMER_DRAG_ENDED: usize = 3;
const TIMER_WATCH: usize = 4;
/// ~60 fps (Windows rounds a user timer up to its clock, so a frame is 15-16 ms).
const FRAME_MS: u32 = 16;
/// A drag donut that never got files and never heard the button go up (hook lost, UAC took the desktop) would leave
/// an invisible window that blocks clicks: it closes itself after this long.
const EMPTY_DRAG_MS: u32 = 10_000;
/// After the button is released a drop gets this long to arrive.
const DROP_GRACE_MS: u32 = 250;

struct Donut {
    hwnd: HWND,
    menu: Menu,
    next_style: Style,
    palette: Palette,
    renderer: Option<Renderer>,
    surface: Option<Surface>,
    provider: Option<FacesProvider>,
    visible: bool,
    dropped: bool,
    drag_reached: bool,
    offers_files: bool,
    focused: bool,
    /// Window position (physical px) and DPI scale of the monitor it was placed on.
    origin: (i32, i32),
    scale: f32,
    animating: bool,
    last_frame: Option<Instant>,
    tracking_leave: bool,
    last_pointer: Option<Pt>,
    last_log: Option<Instant>,
}

thread_local! {
    static DONUT: RefCell<Donut> = RefCell::new(Donut::new());
}

/// Runs `f` on the donut, or returns None when it is busy (re-entered from one of our own Win32 calls).
fn with<R>(f: impl FnOnce(&mut Donut) -> R) -> Option<R> {
    DONUT.with(|d| d.try_borrow_mut().ok().map(|mut d| f(&mut d)))
}

// ------------------------------------------------------------------ public

pub fn init() {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("Filee Rust donut"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        );
        let Ok(hwnd) = hwnd else {
            crate::warn!("donut: cannot create the window");
            return;
        };
        let target: windows::Win32::System::Ole::IDropTarget = super::drop::DropTarget.into();
        if let Err(e) = RegisterDragDrop(hwnd, &target) {
            crate::warn!("donut: RegisterDragDrop failed: {e}");
        }
        with(|d| d.hwnd = hwnd);
    }
}

/// At exit, before OLE and COM are uninitialised: the drop target is revoked and the Direct2D objects released here
/// rather than by thread-local destructors while the process is being torn down.
pub fn shutdown() {
    let _ = DONUT.try_with(|d| {
        if let Ok(mut d) = d.try_borrow_mut() {
            if !d.hwnd.is_invalid() {
                unsafe {
                    let _ = windows::Win32::System::Ole::RevokeDragDrop(d.hwnd);
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(d.hwnd);
                }
                d.hwnd = HWND::default();
            }
            d.renderer = None;
            d.surface = None;
        }
    });
}

/// `EndDraw` reports `D2DERR_RECREATE_TARGET` when the device was lost; the renderer must then be rebuilt.
fn needs_new_renderer(hr: windows::core::HRESULT) -> bool {
    hr == D2DERR_RECREATE_TARGET
}

/// `D2DERR_RECREATE_TARGET` (0x8899000C); the windows crate does not export the constant.
const D2DERR_RECREATE_TARGET: windows::core::HRESULT = windows::core::HRESULT(0x8899000Cu32 as i32);

pub fn set_style(palette: &Palette, donut_settings: &serde_json::Value) {
    with(|d| {
        d.palette = palette.clone();
        d.next_style = Style::from_settings(palette, donut_settings);
        if let Some(r) = &d.renderer {
            r.set_fonts(&palette.fonts);
        }
        // Sizes change the window: applied at the next show, not under an open donut.
        if !d.visible {
            d.menu.style = d.next_style.clone();
        } else {
            d.menu.style.motion = d.next_style.motion;
            d.menu.style.flip_key = d.next_style.flip_key.clone();
            d.menu.style.flip_wheel = d.next_style.flip_wheel;
        }
    });
}

pub fn set_faces_provider(provider: FacesProvider) {
    with(|d| d.provider = Some(provider));
}

pub fn show_for_drag(x: i32, y: i32) {
    with(|d| d.show_for_drag(x, y));
}

pub fn on_drag_gesture_ended() {
    with(|d| d.on_drag_gesture_ended());
}

pub fn show_for_files(x: i32, y: i32, files: Vec<PathBuf>) {
    with(|d| d.show_for_files(x, y, files));
}

pub fn flip() {
    with(|d| {
        if d.visible && d.menu.flip() {
            d.after_input();
        }
    });
}

#[allow(dead_code)]
pub fn is_open() -> bool {
    with(|d| d.visible).unwrap_or(true)
}

#[allow(dead_code)]
pub fn close() {
    with(|d| d.close(true, false));
}

// --------------------------------------------------------- OLE drop target

/// DragEnter: the dragged files (empty when none could be read) and whether the source offers files at all.
pub fn drag_enter(files: Vec<PathBuf>, offered: bool, pt: (i32, i32), allowed: DROPEFFECT) -> DROPEFFECT {
    with(|d| d.drag_enter(files, offered, pt)).map_or(DROPEFFECT_NONE, |e| allow(e, allowed))
}

pub fn drag_over(pt: (i32, i32), allowed: DROPEFFECT) -> DROPEFFECT {
    with(|d| d.drag_over(pt)).map_or(DROPEFFECT_NONE, |e| allow(e, allowed))
}

pub fn drag_leave() {
    with(|d| d.drag_leave());
}

pub fn drop_files(files: Vec<PathBuf>, pt: (i32, i32), allowed: DROPEFFECT) -> DROPEFFECT {
    with(|d| d.drop_files(files, pt)).map_or(DROPEFFECT_NONE, |e| allow(e, allowed))
}

/// Copy only when the source allows copying; never Move (Explorer would delete the originals) or Link.
fn allow(effect: DROPEFFECT, allowed: DROPEFFECT) -> DROPEFFECT {
    if effect == DROPEFFECT_COPY && allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else {
        DROPEFFECT_NONE
    }
}

// -------------------------------------------------------------- controller

impl Donut {
    fn new() -> Donut {
        Donut {
            hwnd: HWND::default(),
            menu: Menu::new(),
            next_style: Style::default(),
            palette: Palette::default(),
            renderer: None,
            surface: None,
            provider: None,
            visible: false,
            dropped: false,
            drag_reached: false,
            offers_files: false,
            focused: false,
            origin: (0, 0),
            scale: 1.0,
            animating: false,
            last_frame: None,
            tracking_leave: false,
            last_pointer: None,
            last_log: None,
        }
    }

    fn ready(&self) -> bool {
        !self.hwnd.0.is_null()
    }

    fn faces_for(&self, files: &[PathBuf]) -> Faces {
        self.provider.as_ref().map(|p| p(files)).unwrap_or_default()
    }

    fn reset_session(&mut self) {
        unsafe {
            for id in [TIMER_EMPTY_DRAG, TIMER_DRAG_ENDED, TIMER_WATCH] {
                let _ = KillTimer(Some(self.hwnd), id);
            }
        }
        self.menu.style = self.next_style.clone();
        self.dropped = false;
        self.drag_reached = false;
        self.offers_files = false;
        self.focused = false;
        self.last_pointer = None;
    }

    /// Drag mode, files not known yet: an invisible window that still catches the drag (spec §2.2.A).
    fn show_for_drag(&mut self, x: i32, y: i32) {
        if !self.ready() {
            return;
        }
        self.reset_session();
        self.menu.reset_for_drag();
        self.menu.mode = Mode::Drag;
        self.place_and_show(x, y, false);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_EMPTY_DRAG, EMPTY_DRAG_MS, None);
        }
    }

    /// Click mode: the files are known, the donut takes the keyboard and closes when it loses it (spec §3.3).
    fn show_for_files(&mut self, x: i32, y: i32, files: Vec<PathBuf>) {
        if !self.ready() || files.is_empty() {
            return;
        }
        self.reset_session();
        let faces = self.faces_for(&files);
        self.menu.load(faces, files, Mode::Click);
        self.menu.play_open();
        self.place_and_show(x, y, true);
        self.focus();
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_WATCH, 100, None);
        }
        self.start_frames();
    }

    /// Centres the window on the cursor (clamped into the work area of the monitor it is on), draws the first frame and
    /// shows it on top of everything without activating it.
    fn place_and_show(&mut self, x: i32, y: i32, activate: bool) {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let area = if GetMonitorInfoW(monitor, &mut info).as_bool() {
                (info.rcWork.left, info.rcWork.top, info.rcWork.right, info.rcWork.bottom)
            } else {
                (i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
            };
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            self.scale = dpi_x.max(96) as f32 / 96.0;
            let side = (self.menu.side() * self.scale as f64).ceil() as i32;
            self.origin = window_position((x, y), side, area);
            if self.surface.as_ref().is_none_or(|s| s.width != side) {
                self.surface = Surface::new(side, side);
            }

            // Click mode takes the keyboard; a drag donut must never steal it from the file manager.
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let wanted = if activate { ex & !(WS_EX_NOACTIVATE.0 as isize) } else { ex | WS_EX_NOACTIVATE.0 as isize };
            if wanted != ex {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, wanted);
            }
            self.draw_frame();
            self.visible = true;
            // Every time: a window that is already topmost doesn't move in z-order when shown again, and Windows may
            // have put it under a normal window meanwhile, where a drop could not reach it.
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                self.origin.0,
                self.origin.1,
                side,
                side,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    /// Foreground rights are only given to the process that has the input; failing that, borrow the foreground
    /// thread's input queue for the call (no simulated keys).
    fn focus(&mut self) {
        unsafe {
            if GetForegroundWindow() != self.hwnd {
                let _ = SetForegroundWindow(self.hwnd);
            }
            let fg = GetForegroundWindow();
            if fg != self.hwnd {
                let (me, other) = (GetCurrentThreadId(), GetWindowThreadProcessId(fg, None));
                if other != 0 && other != me {
                    let _ = windows::Win32::System::Threading::AttachThreadInput(me, other, true);
                    let _ = SetForegroundWindow(self.hwnd);
                    let _ = windows::Win32::System::Threading::AttachThreadInput(me, other, false);
                }
            }
            let _ = SetFocus(Some(self.hwnd));
            self.focused = GetForegroundWindow() == self.hwnd;
        }
    }

    fn close(&mut self, animate: bool, picked: bool) {
        if !self.visible {
            return;
        }
        self.visible = false;
        unsafe {
            for id in [TIMER_EMPTY_DRAG, TIMER_DRAG_ENDED, TIMER_WATCH] {
                let _ = KillTimer(Some(self.hwnd), id);
            }
        }
        self.menu.collapse_group();
        if !picked {
            events::post(AppEvent::Donut(DonutEvent::Closed));
        }
        if animate && self.menu.has_files() {
            self.menu.play_close();
            self.start_frames();
        } else {
            self.hide();
        }
    }

    fn hide(&mut self) {
        self.visible = false;
        self.animating = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_FRAME);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn on_drag_gesture_ended(&mut self) {
        if !self.visible || self.menu.mode != Mode::Drag {
            return;
        }
        if !self.drag_reached && self.last_log.is_none_or(|t| t.elapsed().as_secs() >= 60) {
            self.last_log = Some(Instant::now());
            crate::info!("donut: the drag ended without ever reaching the donut (elevated source, or the drop was elsewhere)");
        }
        if !self.menu.has_files() {
            self.close(false, false); // an invisible window that would swallow clicks
        } else {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_DRAG_ENDED, DROP_GRACE_MS, None);
            }
        }
    }

    // -------------------------------------------------------------- frames

    fn start_frames(&mut self) {
        if !self.animating {
            self.animating = true;
            self.last_frame = None;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_FRAME, FRAME_MS, None);
            }
        }
    }

    fn on_frame(&mut self) {
        let now = Instant::now();
        let dt = self.last_frame.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f64().clamp(0.0, 0.05));
        self.last_frame = Some(now);
        let active = self.menu.advance(dt);
        let hidden = self.process_effects();
        if hidden {
            return;
        }
        self.draw_frame();
        if !active && self.animating {
            // Nothing moves: stop the timer until the next event (saves the CPU while the donut just sits there).
            self.animating = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_FRAME);
            }
        }
    }

    /// Acts on what the menu asked for; true when the window got hidden.
    fn process_effects(&mut self) -> bool {
        let mut hidden = false;
        for effect in self.menu.take_effects() {
            match effect {
                Effect::Invoke { index, sub } => {
                    if let Some(s) = self.menu.preset_at(index, sub) {
                        let picked = DonutEvent::Picked {
                            preset_id: s.preset_id.clone(),
                            action: s.action,
                            files: self.menu.files.clone(),
                        };
                        events::post(AppEvent::Donut(picked));
                        self.close(true, true);
                    }
                }
                Effect::Close => self.close(true, false),
                Effect::Edit { index, sub } => {
                    let id = self.menu.items.get(index).and_then(|s| match sub {
                        Some(k) => s.children.get(k),
                        None => Some(s),
                    });
                    if let Some(s) = id {
                        events::post(AppEvent::Donut(DonutEvent::EditRequested { preset_id: s.preset_id.clone() }));
                    }
                    // Upstream's editor opens over the donut and the donut would lose focus anyway.
                    self.close(false, true);
                    hidden = true;
                }
                Effect::CloseDone => {
                    self.hide();
                    hidden = true;
                }
            }
        }
        hidden
    }

    /// Something changed from outside the frame loop (pointer, key, drag): act on its effects and make sure a frame
    /// follows.
    fn after_input(&mut self) {
        if self.process_effects() || !self.visible && !self.menu.closing {
            return;
        }
        self.start_frames();
    }

    fn draw_frame(&mut self) {
        let Some(surface) = self.surface.as_mut() else { return };
        if self.renderer.is_none() {
            match Renderer::new() {
                Ok(r) => {
                    r.set_fonts(&self.palette.fonts);
                    self.renderer = Some(r);
                }
                Err(e) => {
                    if self.last_log.is_none_or(|t| t.elapsed().as_secs() >= 60) {
                        self.last_log = Some(Instant::now());
                        crate::warn!("donut: Direct2D is unavailable: {e}");
                    }
                }
            }
        }
        let mut lost_target = false;
        if let Some(r) = &self.renderer {
            if let Err(e) = r.draw(surface.dc, surface.width, surface.height, self.scale, &self.menu, &self.palette) {
                crate::warn!("donut: draw failed: {e}");
                lost_target = needs_new_renderer(e.code());
            }
        }
        if lost_target {
            // The render target's device is gone (display change, driver reset): drawing keeps failing until every
            // Direct2D resource is rebuilt, which the next frame does.
            self.renderer = None;
        }
        // Even when drawing failed or has nothing to show, keep the area that must catch drags and clicks.
        surface.apply_backing(&self.menu.backing(), self.menu.center(), self.scale as f64);
        unsafe {
            let pos = POINT { x: self.origin.0, y: self.origin.1 };
            let size = SIZE { cx: surface.width, cy: surface.height };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(surface.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    // ---------------------------------------------------------------- drag

    fn to_dip(&self, screen: (i32, i32)) -> Pt {
        Pt::new((screen.0 - self.origin.0) as f64 / self.scale as f64, (screen.1 - self.origin.1) as f64 / self.scale as f64)
    }

    fn drag_enter(&mut self, files: Vec<PathBuf>, offered: bool, pt: (i32, i32)) -> DROPEFFECT {
        if !self.visible || self.menu.mode != Mode::Drag {
            return DROPEFFECT_NONE;
        }
        self.drag_reached = true;
        self.offers_files = !files.is_empty() || offered;
        if files.is_empty() {
            if self.last_log.is_none_or(|t| t.elapsed().as_secs() >= 60) {
                self.last_log = Some(Instant::now());
                crate::info!("donut: the drag carries no local files (browser files and links are not supported yet)");
            }
            return DROPEFFECT_NONE;
        }
        if !(self.menu.has_files() && same_files(&self.menu.files, &files)) {
            let faces = self.faces_for(&files);
            self.menu.load(faces, files, Mode::Drag);
            self.menu.play_open();
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_EMPTY_DRAG);
            }
        }
        self.drag_over(pt)
    }

    fn drag_over(&mut self, pt: (i32, i32)) -> DROPEFFECT {
        if !self.visible || !self.menu.has_files() {
            return DROPEFFECT_NONE;
        }
        let hit = self.menu.set_external_hit(self.to_dip(pt));
        self.after_input();
        if self.offers_files && self.menu.converts(hit) {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    }

    fn drag_leave(&mut self) {
        if !self.visible || !self.menu.has_files() {
            return;
        }
        let last = self.menu.last_external_at();
        self.menu.external_left(last);
        self.after_input();
    }

    fn drop_files(&mut self, files: Vec<PathBuf>, pt: (i32, i32)) -> DROPEFFECT {
        if !self.visible || !self.menu.has_files() {
            return DROPEFFECT_NONE;
        }
        let hit = self.menu.hit_test(self.to_dip(pt));
        self.dropped = true;
        let converts = self.menu.converts(hit);
        self.menu.collapse_group();
        self.menu.external_left(None);
        if converts {
            let (index, sub) = match hit {
                Hit::Segment(i) => (i, None),
                Hit::SubSegment(i, k) => (i, Some(k)),
                _ => return DROPEFFECT_NONE,
            };
            if let Some(s) = self.menu.preset_at(index, sub) {
                let files = if files.is_empty() { self.menu.files.clone() } else { files };
                events::post(AppEvent::Donut(DonutEvent::Picked { preset_id: s.preset_id.clone(), action: s.action, files }));
                self.close(true, true);
                return DROPEFFECT_COPY;
            }
        }
        self.close(true, false);
        DROPEFFECT_NONE
    }

    // --------------------------------------------------------------- input

    fn click_mode(&self) -> bool {
        self.visible && self.menu.mode == Mode::Click && !self.menu.closing
    }

    fn mouse_move(&mut self, px: (i32, i32)) {
        if !self.click_mode() {
            return;
        }
        let p = self.to_dip((px.0 + self.origin.0, px.1 + self.origin.1));
        self.last_pointer = Some(p);
        if !self.tracking_leave {
            self.tracking_leave = true;
            unsafe {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut t);
            }
        }
        self.menu.pointer_moved(p);
        self.after_input();
    }

    fn mouse_leave(&mut self) {
        self.tracking_leave = false;
        if self.click_mode() {
            let last = self.last_pointer;
            self.menu.pointer_left(last);
            self.after_input();
        }
    }

    fn mouse_button(&mut self, px: (i32, i32), right: bool, down: bool) {
        if !self.click_mode() {
            return;
        }
        let p = self.to_dip((px.0 + self.origin.0, px.1 + self.origin.1));
        unsafe {
            if down && !right {
                SetCapture(self.hwnd);
            } else if !down && !right {
                let _ = ReleaseCapture();
            }
        }
        if down {
            self.menu.pointer_pressed(p, right);
        } else if !right {
            self.menu.pointer_released(p);
        }
        self.after_input();
    }

    fn wheel(&mut self, delta: i16) -> bool {
        if !self.click_mode() || delta == 0 {
            return false;
        }
        let now = unsafe { GetTickCount64() };
        self.menu.flip_from_wheel(now);
        self.after_input();
        true
    }

    fn key(&mut self, vk: u32) -> bool {
        if !self.click_mode() {
            return false;
        }
        let (key, name) = key_from_vk(vk);
        if self.menu.handle_key(key, &name) {
            self.after_input();
            return true;
        }
        if key == Key::Escape {
            self.close(true, false);
            return true;
        }
        false
    }

    /// Lost the keyboard: a click-mode donut closes (clicking elsewhere does this too).
    fn deactivated(&mut self) {
        if self.click_mode() {
            self.close(true, false);
        }
    }

    /// If the donut never got the foreground (Windows refused), losing it can't be noticed: a click elsewhere closes it.
    fn watch(&mut self) {
        if !self.click_mode() {
            return;
        }
        unsafe {
            let foreground = GetForegroundWindow() == self.hwnd;
            self.focused |= foreground;
            if foreground {
                return;
            }
            let pressed = (GetAsyncKeyState(VK_LBUTTON.0 as i32) | GetAsyncKeyState(VK_RBUTTON.0 as i32)) as u16 & 0x8000 != 0;
            if !pressed {
                return;
            }
            let mut cursor = POINT::default();
            if GetCursorPos(&mut cursor).is_ok() {
                let p = self.to_dip((cursor.x, cursor.y));
                if p.dist(self.menu.center()) > self.menu.style.outer + POP_OUT {
                    self.close(true, false);
                }
            }
        }
    }
}

fn same_files(a: &[PathBuf], b: &[PathBuf]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_string_lossy().eq_ignore_ascii_case(&y.to_string_lossy()))
}

/// The key's role in the donut and its hook-style name ("Tab", "F2", "Q", "PageDown") for the flip key.
pub fn key_from_vk(vk: u32) -> (Key, String) {
    let (key, name): (Key, String) = match vk {
        0x25 => (Key::Left, "Left".into()),
        0x27 => (Key::Right, "Right".into()),
        0x26 => (Key::Up, "Up".into()),
        0x28 => (Key::Down, "Down".into()),
        0x0D => (Key::Enter, "Enter".into()),
        0x20 => (Key::Space, "Space".into()),
        0x1B => (Key::Escape, "Escape".into()),
        0x08 => (Key::Backspace, "Backspace".into()),
        0x09 => (Key::Other, "Tab".into()),
        0x21 => (Key::Other, "PageUp".into()),
        0x22 => (Key::Other, "PageDown".into()),
        0x23 => (Key::Other, "End".into()),
        0x24 => (Key::Other, "Home".into()),
        0x2D => (Key::Other, "Insert".into()),
        0x2E => (Key::Other, "Delete".into()),
        0x30..=0x39 | 0x41..=0x5A => (Key::Other, char::from_u32(vk).map(String::from).unwrap_or_default()),
        0x60..=0x69 => (Key::Other, format!("NumPad{}", vk - 0x60)),
        0x70..=0x87 => (Key::Other, format!("F{}", vk - 0x6F)),
        _ => (Key::Other, String::new()),
    };
    (key, name)
}

fn lo(l: LPARAM) -> i32 {
    (l.0 & 0xFFFF) as i16 as i32
}

fn hi(l: LPARAM) -> i32 {
    ((l.0 >> 16) & 0xFFFF) as i16 as i32
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // A drag donut never takes focus from the file manager; a click-mode one does.
        WM_MOUSEACTIVATE => {
            let drag = with(|d| d.menu.mode == Mode::Drag).unwrap_or(true);
            return LRESULT(if drag { MA_NOACTIVATE } else { MA_ACTIVATE } as isize);
        }
        WM_MOUSEMOVE => {
            with(|d| d.mouse_move((lo(lparam), hi(lparam))));
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|d| d.mouse_leave());
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            let (right, down) = (matches!(msg, WM_RBUTTONDOWN | WM_RBUTTONUP), matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN));
            with(|d| d.mouse_button((lo(lparam), hi(lparam)), right, down));
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
            if with(|d| d.wheel(delta)).unwrap_or(false) {
                return LRESULT(0);
            }
        }
        WM_KEYDOWN => {
            if with(|d| d.key(wparam.0 as u32)).unwrap_or(false) {
                return LRESULT(0);
            }
        }
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                with(|d| d.deactivated());
            }
            // The default handling gives the keyboard focus to the window when it is activated.
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_FRAME => {
                    with(|d| d.on_frame());
                }
                TIMER_EMPTY_DRAG => {
                    let _ = KillTimer(Some(hwnd), TIMER_EMPTY_DRAG);
                    with(|d| {
                        if d.visible && d.menu.mode == Mode::Drag && !d.menu.has_files() {
                            d.close(false, false);
                        }
                    });
                }
                TIMER_DRAG_ENDED => {
                    let _ = KillTimer(Some(hwnd), TIMER_DRAG_ENDED);
                    with(|d| {
                        if d.visible && !d.dropped && d.menu.mode == Mode::Drag {
                            d.close(true, false);
                        }
                    });
                }
                TIMER_WATCH => {
                    with(|d| d.watch());
                }
                _ => {}
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_lost_render_target_drops_the_renderer() {
        assert!(needs_new_renderer(D2DERR_RECREATE_TARGET));
        assert!(!needs_new_renderer(windows::core::HRESULT(0)));
        assert!(!needs_new_renderer(windows::Win32::Foundation::E_FAIL));
    }

    #[test]
    fn key_names_match_the_hook_names() {
        assert_eq!(key_from_vk(0x09), (Key::Other, "Tab".to_string()));
        assert_eq!(key_from_vk(0x51), (Key::Other, "Q".to_string()));
        assert_eq!(key_from_vk(0x35), (Key::Other, "5".to_string()));
        assert_eq!(key_from_vk(0x71), (Key::Other, "F2".to_string()));
        assert_eq!(key_from_vk(0x87), (Key::Other, "F24".to_string()));
        assert_eq!(key_from_vk(0x62), (Key::Other, "NumPad2".to_string()));
        assert_eq!(key_from_vk(0x22), (Key::Other, "PageDown".to_string()));
        assert_eq!(key_from_vk(0x28), (Key::Down, "Down".to_string()));
        assert_eq!(key_from_vk(0x1B).0, Key::Escape);
        assert_eq!(key_from_vk(0x08).0, Key::Backspace);
        assert_eq!(key_from_vk(0xFF).1, "");
    }

    #[test]
    fn same_files_ignores_case_and_needs_the_same_order() {
        let a = vec![PathBuf::from(r"C:\A\x.png"), PathBuf::from(r"C:\A\y.png")];
        let b = vec![PathBuf::from(r"c:\a\X.PNG"), PathBuf::from(r"C:\A\Y.png")];
        assert!(same_files(&a, &b));
        assert!(!same_files(&a, &b[..1]));
        let swapped = vec![b[1].clone(), b[0].clone()];
        assert!(!same_files(&a, &swapped));
    }

    #[test]
    fn only_copy_is_ever_returned() {
        let all = DROPEFFECT(DROPEFFECT_COPY.0 | 2 | 4);
        assert_eq!(allow(DROPEFFECT_COPY, all), DROPEFFECT_COPY);
        assert_eq!(allow(DROPEFFECT_COPY, DROPEFFECT(2)), DROPEFFECT_NONE);
        assert_eq!(allow(DROPEFFECT_NONE, all), DROPEFFECT_NONE);
    }
}
