//! Progress / result toasts (spec 03 §1): one layered window at the bottom-right of the primary work area
//! that stacks a card per job (newest on top). Cards are drawn by `render.rs` and shown with `UpdateLayeredWindow`.
//! Everything runs on the UI thread; the window state lives in a thread-local.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, ScreenToClient, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::i18n;
use super::render::{
    BtnKind, BtnSpec, Canvas, CardLayout, CardModel, Gfx, Icon, Interaction, Progress, Surface, CARD_GAP, CARD_W, EDGE,
    WINDOW_W,
};
use crate::model::{JobState, JobStatus, Palette};

const TIMER: usize = 1;
/// `WM_MOUSELEAVE` (Win32_UI_Controls is not enabled).
const WM_MOUSELEAVE: u32 = 0x02A3;
const ENTRY_SECS: f32 = 0.35;
const SLIDE_DIP: f32 = 40.0;
const KEEP_OK: Duration = Duration::from_secs(5);
const KEEP_ERRORS: Duration = Duration::from_secs(15);
const SCREEN_MARGIN_PX: i32 = 12;

/// What the Cancel and Retry buttons call. `core` registers these once; a button is only shown when its
/// callback is set (the toast cannot cancel or rerun anything itself).
pub struct JobActions {
    /// Ask the running job `id` to stop (`filee-cli` gets Ctrl+Break / is terminated).
    pub cancel: Box<dyn Fn(u64) + Send + Sync>,
    /// Rerun the failed files of job `id` with the same preset; returns nothing, the new job arrives as a normal
    /// `AppEvent::Job`. The toast removes the old card itself.
    pub retry: Box<dyn Fn(u64) + Send + Sync>,
}

static ACTIONS: OnceLock<JobActions> = OnceLock::new();

#[allow(dead_code)] // core calls this once cancel / retry exist
pub fn set_job_actions(actions: JobActions) {
    let _ = ACTIONS.set(actions);
}

struct Card {
    job: JobStatus,
    born: Instant,
    finished: Option<Instant>,
}

/// A card as last drawn: where it is, for hit testing.
struct Placed {
    id: u64,
    layout: CardLayout,
    top: f32,
}

struct Toast {
    hwnd: HWND,
    palette: Palette,
    cards: Vec<Card>,
    gfx: Option<Gfx>,
    canvas: Option<Canvas>,
    surface: Option<Surface>,
    placed: Vec<Placed>,
    hover: Option<(u64, BtnKind)>,
    pressed: Option<(u64, BtnKind)>,
    tracking: bool,
    /// The last frame had motion (entry slide, moving bar): the timer must keep redrawing.
    animating: bool,
    /// Jobs whose card the user closed while running; their later updates stay hidden.
    dismissed: Vec<u64>,
    scale: f32,
    shown: bool,
    epoch: Instant,
}

thread_local! {
    static TOAST: RefCell<Option<Toast>> = const { RefCell::new(None) };
}

/// Creates the (hidden) toast window. Call once on the UI thread.
pub fn init() {
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(toast_proc),
            hInstance: instance.into(),
            lpszClassName: w!("FileeLiteToast"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("FileeLiteToast"),
            w!("Filee Rust toast"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    match hwnd {
        Ok(hwnd) => {
            TOAST.with(|t| {
                *t.borrow_mut() = Some(Toast {
                    hwnd,
                    palette: Palette::default(),
                    cards: Vec::new(),
                    gfx: None,
                    canvas: None,
                    surface: None,
                    placed: Vec::new(),
                    hover: None,
                    pressed: None,
                    tracking: false,
                    animating: false,
                    dismissed: Vec::new(),
                    scale: 1.0,
                    shown: false,
                    epoch: Instant::now(),
                })
            });
        }
        Err(e) => crate::error!("Toast window failed: {e}"),
    }
}

/// At exit, before COM is uninitialised: releases the Direct2D/DirectWrite objects here instead of in the thread-local
/// destructors that run while the process is already being torn down.
pub fn shutdown() {
    let _ = TOAST.try_with(|t| t.try_borrow_mut().map(|mut t| t.take()));
}

/// The language changed: the text layouts are rebuilt with its locale (fonts fall back per language).
pub fn language_changed() {
    with(|t| t.gfx = None);
}

/// New colours / fonts / motion setting; redraws what is on screen.
pub fn set_palette(palette: &Palette) {
    with(|t| {
        if t.palette.fonts != palette.fonts {
            t.gfx = None;
        }
        t.palette = palette.clone();
        if !t.cards.is_empty() {
            refresh(t);
        }
    });
}

/// Adds the card for `job.id` (newest on top) or updates the existing one.
pub fn show(job: &JobStatus) {
    with(|t| {
        if t.dismissed.contains(&job.id) {
            return;
        }
        let now = Instant::now();
        let finished = (job.state != JobState::Running).then_some(now);
        match t.cards.iter_mut().find(|c| c.job.id == job.id) {
            Some(card) => {
                if card.finished.is_none() {
                    card.finished = finished;
                }
                card.job = job.clone();
            }
            None => t.cards.insert(0, Card { job: job.clone(), born: now, finished }),
        }
        refresh(t);
    });
}

fn with(f: impl FnOnce(&mut Toast)) {
    TOAST.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(t) = guard.as_mut() {
                f(t);
            }
        }
    });
}

// ---- model ----------------------------------------------------------------------------------------------

/// What a job's card says and offers (spec §1.3). `cancel` / `retry`: whether the callbacks exist.
fn model_for(job: &JobStatus, phase: f32, cancel: bool, retry: bool) -> CardModel {
    let title = if job.title.is_empty() { job.preset_id.clone() } else { job.title.clone() };
    let has_output = !job.outputs.is_empty();
    let button = |kind, key: &str, icon, accent| BtnSpec { kind, label: i18n::t(key), icon, accent };
    let mut buttons = Vec::new();
    let (status, progress) = match job.state {
        JobState::Running => {
            if cancel {
                buttons.push(button(BtnKind::Cancel, "toast.cancel", None, false));
            }
            if job.total > 1 {
                let fraction = job.done as f32 / job.total as f32;
                (format!("{:.0}%", fraction * 100.0), Progress::Fraction(fraction))
            } else {
                (String::new(), Progress::Indeterminate(phase))
            }
        }
        JobState::Completed => (i18n::t("toast.done"), Progress::None),
        JobState::CompletedWithErrors => {
            let (failed, total) = (job.failed.to_string(), job.total.to_string());
            (i18n::format("toast.done_errors", &[&failed, &total]), Progress::None)
        }
        JobState::Failed => (i18n::t("toast.failed"), Progress::None),
        JobState::Cancelled => (i18n::t("toast.cancelled"), Progress::None),
    };
    let can_retry = retry && matches!(job.state, JobState::CompletedWithErrors | JobState::Failed) && job.failed > 0;
    if can_retry {
        buttons.push(button(BtnKind::Retry, "common.retry", Some(Icon::Refresh), false));
    }
    if has_output {
        if job.state != JobState::Running {
            buttons.push(button(BtnKind::Open, "home.open_file", None, false));
        }
        buttons.push(button(BtnKind::Folder, "toast.open_folder", Some(Icon::Folder), true));
    }
    CardModel { id: job.id, title, status, progress, buttons, errors: job.errors.join("\n") }
}

/// How long a finished card stays: 15 s with failures, 5 s otherwise; running cards stay.
fn keep_for(job: &JobStatus) -> Option<Duration> {
    if job.state == JobState::Running {
        return None;
    }
    let failures = job.failed > 0 || !job.errors.is_empty() || matches!(job.state, JobState::Failed | JobState::CompletedWithErrors);
    Some(if failures { KEEP_ERRORS } else { KEEP_OK })
}

/// Standard easeOutBack (overshoot 1.70158), t in 0..1.
fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    1.0 + t * t * ((1.70158 + 1.0) * t + 1.70158)
}

// ---- layout, drawing, presenting ----------------------------------------------------------------------

fn primary_work_area() -> (windows::Win32::Foundation::RECT, f32) {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (info.rcWork, dx.max(96) as f32 / 96.0)
    }
}

/// The timer: redraw only for motion or when a finished card is due to go.
fn tick(t: &mut Toast) {
    let now = Instant::now();
    let due = t.cards.iter().any(|c| match (c.finished, keep_for(&c.job)) {
        (Some(done), Some(keep)) => now.duration_since(done) >= keep,
        _ => false,
    });
    if due || t.animating {
        refresh(t);
    }
}

fn refresh(t: &mut Toast) {
    let now = Instant::now();
    t.cards.retain(|c| match (c.finished, keep_for(&c.job)) {
        (Some(done), Some(keep)) => now.duration_since(done) < keep,
        _ => true,
    });
    if t.cards.is_empty() {
        t.placed.clear();
        t.hover = None;
        t.pressed = None;
        unsafe {
            let _ = KillTimer(Some(t.hwnd), TIMER);
            let _ = ShowWindow(t.hwnd, SW_HIDE);
        }
        t.shown = false;
        t.animating = false;
        // Nothing on screen: give the graphics memory back (the resident process stays small).
        t.canvas = None;
        t.surface = None;
        t.gfx = None;
        return;
    }
    if let Err(e) = draw_and_present(t, now) {
        crate::warn!("Toast draw failed: {e}");
        t.canvas = None; // device lost: rebuild next time
        t.surface = None;
    }
}

fn draw_and_present(t: &mut Toast, now: Instant) -> windows::core::Result<()> {
    let (work, scale) = primary_work_area();
    t.scale = scale;
    if t.gfx.is_none() {
        t.gfx = Some(Gfx::new(&t.palette.fonts, &i18n::current())?);
        t.canvas = None;
    }
    let gfx = t.gfx.as_mut().expect("gfx");
    let reduce = t.palette.reduce_motion;
    let phase = if reduce { 0.35 } else { (t.epoch.elapsed().as_secs_f32() % 1.4) / 1.4 };
    let (cancel, retry) = (ACTIONS.get().is_some(), ACTIONS.get().is_some());

    let models: Vec<CardModel> = t.cards.iter().map(|c| model_for(&c.job, phase, cancel, retry)).collect();
    let layouts: Vec<CardLayout> = models.iter().map(|m| gfx.measure(m)).collect();
    let height_dip = 2.0 * EDGE
        + layouts.iter().map(|l| l.height).sum::<f32>()
        + CARD_GAP * (layouts.len().saturating_sub(1)) as f32;

    let w_px = (WINDOW_W * scale).ceil() as i32;
    let h_px = (height_dip * scale).ceil() as i32;
    if t.surface.as_ref().map(|s| (s.w, s.h)) != Some((w_px, h_px)) {
        t.surface = Surface::new(w_px, h_px);
    }
    let Some(surface) = t.surface.as_ref() else { return Err(windows::core::Error::from_hresult(windows::core::HRESULT(-1))) };
    if t.canvas.is_none() {
        t.canvas = Some(Canvas::new(gfx)?);
    }
    let canvas = t.canvas.as_ref().expect("canvas");

    canvas.begin(surface, scale)?;
    t.placed.clear();
    let mut y = EDGE;
    let mut animating = false;
    for ((card, model), layout) in t.cards.iter().zip(&models).zip(&layouts) {
        let age = now.duration_since(card.born).as_secs_f32();
        let (dx, mul) = if reduce || age >= ENTRY_SECS {
            (0.0, 1.0)
        } else {
            animating = true;
            let e = ease_out_back(age / ENTRY_SECS);
            (SLIDE_DIP * (1.0 - e), e.clamp(0.0, 1.0))
        };
        let ui = Interaction {
            hover: t.hover.filter(|(id, _)| *id == card.job.id).map(|(_, k)| k),
            pressed: t.pressed.filter(|(id, _)| *id == card.job.id).map(|(_, k)| k),
        };
        canvas.card(gfx, &t.palette, model, layout, (EDGE + dx, y), scale, mul, ui);
        t.placed.push(Placed { id: card.job.id, layout: layout.clone(), top: y });
        y += layout.height + CARD_GAP;
    }
    canvas.end()?;
    animating |= !reduce && models.iter().any(|m| matches!(m.progress, Progress::Indeterminate(_)));

    let size = SIZE { cx: w_px, cy: h_px };
    let origin = POINT { x: work.right - w_px - SCREEN_MARGIN_PX, y: work.bottom - h_px - SCREEN_MARGIN_PX };
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
    unsafe {
        UpdateLayeredWindow(
            t.hwnd,
            None,
            Some(&origin),
            Some(&size),
            Some(surface.dc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )?;
        if !t.shown {
            let _ = SetWindowPos(t.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            t.shown = true;
        }
        SetTimer(Some(t.hwnd), TIMER, if animating { 16 } else { 250 }, None);
        t.animating = animating;
    }
    Ok(())
}

// ---- window procedure and actions ---------------------------------------------------------------------

enum Action {
    Cancel(u64),
    Retry(u64),
    Open(PathBuf),
    Reveal(PathBuf),
}

/// The button (and card) under a client-pixel point.
fn hit(t: &Toast, x: i32, y: i32) -> Option<(u64, Option<BtnKind>)> {
    let (dx, dy) = (x as f32 / t.scale - EDGE, y as f32 / t.scale);
    t.placed.iter().find(|p| dy >= p.top && dy < p.top + p.layout.height && dx >= 0.0 && dx < CARD_W).map(|p| {
        let button = p.layout.hit(dx, dy - p.top);
        (p.id, button)
    })
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn toast_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            let mut point = POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut point);
            let mut over_card = true;
            with(|t| over_card = hit(t, point.x, point.y).is_some());
            return LRESULT(if over_card { HTCLIENT as isize } else { HTTRANSPARENT as isize });
        }
        WM_SETCURSOR => {
            let mut hand = false;
            with(|t| hand = matches!(t.hover, Some((_, _))));
            let cursor = LoadCursorW(None, if hand { IDC_HAND } else { IDC_ARROW }).unwrap_or_default();
            SetCursor(Some(cursor));
            return LRESULT(1);
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_point(lparam);
            with(|t| {
                let over = hit(t, x, y).and_then(|(id, b)| b.map(|b| (id, b)));
                if !t.tracking {
                    let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    if TrackMouseEvent(&mut track).is_ok() {
                        t.tracking = true;
                    }
                }
                if over != t.hover {
                    t.hover = over;
                    refresh(t);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(|t| {
                t.tracking = false;
                if t.hover.take().is_some() {
                    refresh(t);
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lparam_point(lparam);
            with(|t| {
                t.pressed = hit(t, x, y).and_then(|(id, b)| b.map(|b| (id, b)));
                if t.pressed.is_some() {
                    SetCapture(hwnd);
                    refresh(t);
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = lparam_point(lparam);
            let mut action = None;
            with(|t| {
                let pressed = t.pressed.take();
                let _ = ReleaseCapture();
                let released = hit(t, x, y).and_then(|(id, b)| b.map(|b| (id, b)));
                if let (Some(p), Some(r)) = (pressed, released) {
                    if p == r {
                        action = click(t, r.0, r.1);
                    }
                }
                refresh(t);
            });
            match action {
                Some(Action::Cancel(id)) => ACTIONS.get().iter().for_each(|a| (a.cancel)(id)),
                Some(Action::Retry(id)) => ACTIONS.get().iter().for_each(|a| (a.retry)(id)),
                Some(Action::Open(path)) => open_file(&path),
                Some(Action::Reveal(path)) => reveal_in_explorer(&path),
                None => {}
            }
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER => {
            with(tick);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Applies a click on `kind` of job `id`; returns what must happen outside the borrow.
fn click(t: &mut Toast, id: u64, kind: BtnKind) -> Option<Action> {
    let index = t.cards.iter().position(|c| c.job.id == id)?;
    let first_output = t.cards[index].job.outputs.first().cloned();
    match kind {
        BtnKind::Close => {
            let closed = t.cards.remove(index);
            if closed.job.state == JobState::Running {
                t.dismissed.push(id);
                // Keep the newest 64.
                let excess = t.dismissed.len().saturating_sub(64);
                t.dismissed.drain(..excess);
            }
            t.hover = None;
            None
        }
        BtnKind::Cancel => Some(Action::Cancel(id)),
        BtnKind::Retry => {
            t.cards.remove(index);
            t.hover = None;
            Some(Action::Retry(id))
        }
        BtnKind::Open => first_output.map(Action::Open),
        BtnKind::Folder => first_output.map(Action::Reveal),
    }
}

fn open_file(path: &Path) {
    let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let result = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        if (result.0 as isize) <= 32 {
            crate::warn!("Could not open {} (code {})", path.display(), result.0 as isize);
        }
    }
}

/// Explorer window with the file selected (its folder when the file is gone).
fn reveal_in_explorer(path: &Path) {
    let target = if path.exists() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    let wide: Vec<u16> = target.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut pidl = std::ptr::null_mut();
        if SHParseDisplayName(PCWSTR(wide.as_ptr()), None::<&windows::Win32::System::Com::IBindCtx>, &mut pidl, 0, None).is_ok() {
            let selected = SHOpenFolderAndSelectItems(pidl, None, 0);
            ILFree(Some(pidl));
            if selected.is_ok() {
                return;
            }
        }
    }
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", target.display())).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::JobStatus;

    fn job(state: JobState) -> JobStatus {
        JobStatus {
            id: 1,
            title: "photo.png → WebP".into(),
            preset_id: "to-webp".into(),
            total: 1,
            done: 0,
            failed: 0,
            state,
            outputs: vec![],
            errors: vec![],
        }
    }

    fn labels(m: &CardModel) -> Vec<BtnKind> {
        m.buttons.iter().map(|b| b.kind).collect()
    }

    fn init_text() {
        i18n::init("zh-TW");
    }

    #[test]
    fn running_card_shows_percent_for_batches_and_cancel_when_wired() {
        init_text();
        let mut j = job(JobState::Running);
        j.total = 4;
        j.done = 1;
        let m = model_for(&j, 0.0, true, true);
        assert_eq!(m.status, "25%");
        assert_eq!(m.progress, Progress::Fraction(0.25));
        assert_eq!(labels(&m), [BtnKind::Cancel]);
        assert_eq!(m.buttons[0].label, "取消");
        assert!(model_for(&j, 0.0, false, false).buttons.is_empty());
        // A single file has no per-file progress: indeterminate.
        let single = model_for(&job(JobState::Running), 0.5, false, false);
        assert_eq!(single.progress, Progress::Indeterminate(0.5));
        assert_eq!(single.status, "");
    }

    #[test]
    fn finished_cards_follow_the_state_table() {
        init_text();
        let mut ok = job(JobState::Completed);
        ok.outputs = vec![PathBuf::from(r"C:\out\photo.webp")];
        let m = model_for(&ok, 0.0, true, true);
        assert_eq!(m.status, "完成");
        assert_eq!(m.progress, Progress::None);
        assert_eq!(labels(&m), [BtnKind::Open, BtnKind::Folder]);
        assert!(m.buttons[1].accent);
        assert_eq!(m.buttons[1].label, "檢視");

        let mut partial = job(JobState::CompletedWithErrors);
        partial.total = 3;
        partial.failed = 1;
        partial.errors = vec!["bad.png — 不支援此檔案類型".into()];
        let m = model_for(&partial, 0.0, true, true);
        assert_eq!(m.status, "3 個中有 1 個失敗");
        assert_eq!(labels(&m), [BtnKind::Retry]);
        assert_eq!(m.errors, "bad.png — 不支援此檔案類型");
        assert!(model_for(&partial, 0.0, true, false).buttons.is_empty());

        let mut failed = job(JobState::Failed);
        failed.failed = 1;
        assert_eq!(model_for(&failed, 0.0, true, true).status, "轉換失敗");
        assert_eq!(model_for(&job(JobState::Cancelled), 0.0, true, true).status, "已取消");
    }

    #[test]
    fn keep_times_are_5s_ok_15s_failures_and_never_while_running() {
        assert_eq!(keep_for(&job(JobState::Running)), None);
        assert_eq!(keep_for(&job(JobState::Completed)), Some(KEEP_OK));
        assert_eq!(keep_for(&job(JobState::Cancelled)), Some(KEEP_OK));
        let mut failed = job(JobState::Failed);
        failed.failed = 1;
        assert_eq!(keep_for(&failed), Some(KEEP_ERRORS));
        assert_eq!(keep_for(&job(JobState::CompletedWithErrors)), Some(KEEP_ERRORS));
    }

    #[test]
    fn entry_easing_overshoots_and_lands_on_one() {
        assert!((ease_out_back(0.0)).abs() < 1e-6);
        assert!((ease_out_back(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_out_back(0.6) > 1.0);
        assert!((ease_out_back(2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn click_on_close_and_retry_removes_the_card() {
        let mut t = Toast {
            hwnd: HWND(std::ptr::null_mut()),
            palette: Palette::default(),
            cards: vec![
                Card { job: JobStatus { id: 1, ..job(JobState::Failed) }, born: Instant::now(), finished: None },
                Card { job: JobStatus { id: 2, outputs: vec![PathBuf::from(r"C:\a.png")], ..job(JobState::Completed) }, born: Instant::now(), finished: None },
            ],
            gfx: None,
            canvas: None,
            surface: None,
            placed: vec![],
            hover: None,
            pressed: None,
            tracking: false,
            animating: false,
            dismissed: Vec::new(),
            scale: 1.0,
            shown: false,
            epoch: Instant::now(),
        };
        assert!(matches!(click(&mut t, 2, BtnKind::Folder), Some(Action::Reveal(p)) if p == PathBuf::from(r"C:\a.png")));
        assert!(matches!(click(&mut t, 2, BtnKind::Open), Some(Action::Open(_))));
        assert!(matches!(click(&mut t, 1, BtnKind::Cancel), Some(Action::Cancel(1))));
        assert!(matches!(click(&mut t, 1, BtnKind::Retry), Some(Action::Retry(1))));
        assert_eq!(t.cards.len(), 1);
        assert!(click(&mut t, 2, BtnKind::Close).is_none());
        assert!(t.cards.is_empty());
    }

    #[test]
    fn open_button_yields_to_a_long_status_but_stays_on_a_short_one() {
        init_text();
        let mut gfx = Gfx::new(&["Segoe UI".to_string()], "zh-TW").unwrap();
        let outputs = vec![PathBuf::from(r"C:\out.png")];
        let mut partial = JobStatus { total: 5, failed: 2, outputs: outputs.clone(), ..job(JobState::CompletedWithErrors) };
        partial.errors = vec!["x — y".into()];
        let kinds = |gfx: &mut Gfx, j: &JobStatus| -> Vec<BtnKind> {
            let layout = gfx.measure(&model_for(j, 0.0, true, true));
            layout.buttons.iter().map(|b| b.0).collect()
        };
        assert_eq!(kinds(&mut gfx, &partial), [BtnKind::Retry, BtnKind::Folder]);
        let ok = JobStatus { outputs, ..job(JobState::Completed) };
        assert_eq!(kinds(&mut gfx, &ok), [BtnKind::Open, BtnKind::Folder]);
    }

    #[test]
    fn closing_a_running_card_hides_its_later_updates() {
        let mut t = Toast {
            hwnd: HWND(std::ptr::null_mut()),
            palette: Palette::default(),
            cards: vec![Card { job: job(JobState::Running), born: Instant::now(), finished: None }],
            gfx: None,
            canvas: None,
            surface: None,
            placed: vec![],
            hover: None,
            pressed: None,
            tracking: false,
            animating: false,
            dismissed: Vec::new(),
            scale: 1.0,
            shown: false,
            epoch: Instant::now(),
        };
        assert!(click(&mut t, 1, BtnKind::Close).is_none());
        assert_eq!(t.dismissed, [1]);
    }

    // ---- previews: draw cards into memory and save PNGs (no window, no screen capture) ----------------------

    fn save_png(path: &Path, w: i32, h: i32, bgra: &[u8]) -> windows::core::Result<()> {
        use windows::Win32::Graphics::Imaging::*;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        unsafe {
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            let stream = factory.CreateStream()?;
            stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), 0x4000_0000)?; // GENERIC_WRITE
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let (mut frame, mut options) = (None, None);
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame = frame.expect("frame");
            frame.Initialize(options.as_ref())?;
            frame.SetSize(w as u32, h as u32)?;
            let mut format = GUID_WICPixelFormat32bppPBGRA;
            frame.SetPixelFormat(&mut format)?;
            frame.WritePixels(h as u32, (w * 4) as u32, bgra)?;
            frame.Commit()?;
            encoder.Commit()
        }
    }

    /// Draws `jobs` as a stack at `scale`, composites onto a page-coloured background and saves a PNG.
    fn preview(name: &str, jobs: &[JobStatus], pal: &Palette, scale: f32, hover: Option<BtnKind>) -> (i32, i32) {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut gfx = Gfx::new(&pal.fonts, &i18n::current()).expect("gfx");
        let models: Vec<CardModel> = jobs.iter().map(|j| model_for(j, 0.4, true, true)).collect();
        let layouts: Vec<CardLayout> = models.iter().map(|m| gfx.measure(m)).collect();
        let height = 2.0 * EDGE + layouts.iter().map(|l| l.height).sum::<f32>() + CARD_GAP * (layouts.len() - 1) as f32;
        let (w, h) = ((WINDOW_W * scale).ceil() as i32, (height * scale).ceil() as i32);
        let surface = Surface::new(w, h).expect("surface");
        let canvas = Canvas::new(&gfx).expect("canvas");
        canvas.begin(&surface, scale).expect("begin");
        let mut y = EDGE;
        for (model, layout) in models.iter().zip(&layouts) {
            let ui = Interaction { hover, pressed: None };
            canvas.card(&mut gfx, pal, model, layout, (EDGE, y), scale, 1.0, ui);
            y += layout.height + CARD_GAP;
        }
        canvas.end().expect("end");

        // Premultiplied BGRA over an opaque background, so the PNG reads on any viewer.
        let (bg_r, bg_g, bg_b) = if pal.dark { (0x1B, 0x1B, 0x19) } else { (0xF6, 0xF5, 0xF1) };
        let mut out = surface.pixels().to_vec();
        for px in out.chunks_exact_mut(4) {
            let inv = 255 - u32::from(px[3]);
            px[0] = (u32::from(px[0]) + bg_b * inv / 255) as u8;
            px[1] = (u32::from(px[1]) + bg_g * inv / 255) as u8;
            px[2] = (u32::from(px[2]) + bg_r * inv / 255) as u8;
            px[3] = 255;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("toast-preview");
        std::fs::create_dir_all(&dir).unwrap();
        save_png(&dir.join(name), w, h, &out).expect("png");
        // Sanity: something opaque was drawn (the card body) and the corner is transparent-ish.
        assert!(surface.pixels().chunks_exact(4).any(|p| p[3] == 255));
        assert!(layouts.iter().all(|l| l.height > 60.0));
        (w, h)
    }

    fn sample_jobs() -> Vec<JobStatus> {
        let mut running = job(JobState::Running);
        running.id = 3;
        running.title = "3 個檔案 → PDF".into();
        running.total = 3;
        running.done = 1;
        let mut ok = job(JobState::Completed);
        ok.id = 2;
        ok.outputs = vec![PathBuf::from(r"C:\out\photo.webp")];
        let mut partial = job(JobState::CompletedWithErrors);
        partial.id = 1;
        partial.title = "5 個檔案 → PNG".into();
        partial.total = 5;
        partial.failed = 2;
        partial.outputs = vec![PathBuf::from(r"C:\out\a.png")];
        partial.errors = vec![
            "broken.heic — 不支援此檔案類型".into(),
            "empty.docx — 檔案是空的（0 位元組）。用右鍵選單「新增」建立的檔案，要在對應的應用程式中開啟並儲存後才有內容".into(),
        ];
        vec![running, ok, partial]
    }

    #[test]
    fn preview_light_zh_tw() {
        init_text();
        let pal = Palette { fonts: vec!["Microsoft JhengHei UI".into(), "Segoe UI".into()], ..Palette::default() };
        let (w, h) = preview("light-zh-TW.png", &sample_jobs(), &pal, 1.0, None);
        assert_eq!(w, 360);
        assert!(h > 250 && h < 700, "{h}");
    }

    #[test]
    fn preview_dark_zh_tw_hover_150_percent() {
        init_text();
        let pal = Palette {
            dark: true,
            surface: 0xFF262624,
            surface_alt: 0xFF31312E,
            text: 0xFFF1EFE8,
            text_muted: 0xFFAEACA4,
            border: 0x1CFFFFFF,
            danger: 0xFFF09595,
            fonts: vec!["Microsoft JhengHei UI".into(), "Segoe UI".into()],
            ..Palette::default()
        };
        let (w, _) = preview("dark-zh-TW-150.png", &sample_jobs(), &pal, 1.5, Some(BtnKind::Folder));
        assert_eq!(w, 540);
    }
}
