//! Filee Lite: the resident part of Filee in Rust. Gestures, donut, toast and tray live here; conversions run
//! in upstream's `filee-cli.exe`; settings and interactive tools open upstream's `Filee.exe --hosted` on demand.
//!
//! ```text
//! filee-lite.exe                     start resident (tray); a second start opens upstream's settings
//! filee-lite.exe --background        start resident without opening anything (sign-in)
//! filee-lite.exe --convert a b …     open the donut for these files (Explorer verb; forwarded to the running one)
//! filee-lite.exe --convert-list f    same, one path per line in a UTF-8 file
//! filee-lite.exe --settings          open upstream's settings window
//! filee-lite.exe --quit              ask the running one to exit
//! filee-lite.exe --selftest          check the install (app folder, CLI, data) and exit; prints a report
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod donut;
mod events;
mod input;
mod log;
mod model;
mod paths;
mod selftest;
mod single;
mod ui;

use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

use events::AppEvent;
use model::{DonutEvent, InputEvent, SliceAction};

/// Class of the hidden message window; later launches find it to forward their arguments.
pub const MAIN_CLASS: windows::core::PCWSTR = w!("FileeLiteMain");
const TIMER_DATA: usize = 1;
/// Explorer starts one `--convert` per selected file: collect them for a moment, then open the donut once.
const TIMER_CONVERT: usize = 2;
const CONVERT_DEBOUNCE_MS: u32 = 350;
/// Checks whether the weekly engine update is due (core::engine_update), first ten minutes after start.
const TIMER_ENGINES: usize = 3;
const ENGINES_CHECK_MS: u32 = 10 * 60 * 1000;

thread_local! {
    static PENDING_CONVERT: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
    static MAIN_HWND: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub background: bool,
    pub settings: bool,
    pub quit: bool,
    pub selftest: bool,
    pub convert: Vec<PathBuf>,
}

pub fn parse_args(args: &[String]) -> Args {
    let mut out = Args::default();
    let mut collecting = false;
    let mut expect_list = false;
    for arg in args {
        if expect_list {
            expect_list = false;
            if let Ok(text) = std::fs::read_to_string(arg) {
                let text = text.trim_start_matches('\u{feff}');
                out.convert.extend(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from));
            }
            continue;
        }
        match arg.to_ascii_lowercase().as_str() {
            "--background" => out.background = true,
            "--settings" => out.settings = true,
            "--quit" => out.quit = true,
            "--selftest" => out.selftest = true,
            "--convert" => collecting = true,
            "--convert-list" => expect_list = true,
            _ if collecting && !arg.starts_with("--") => out.convert.push(PathBuf::from(arg)),
            _ => {}
        }
    }
    out.convert.retain(|p| p.exists());
    out
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&raw);
    if args.selftest {
        std::process::exit(selftest::run());
    }

    // A later launch hands its request to the running Filee Lite and ends.
    let instance = single::acquire();
    if !instance.is_first {
        let forwarded = if args.quit {
            single::forward(&["--quit".to_string()])
        } else if !args.convert.is_empty() {
            let mut a = vec!["--convert".to_string()];
            a.extend(args.convert.iter().map(|p| p.to_string_lossy().into_owned()));
            single::forward(&a)
        } else if args.background {
            true
        } else {
            single::forward(&["--settings".to_string()])
        };
        std::process::exit(if forwarded { 0 } else { 1 });
    }
    if args.quit {
        return; // nothing running
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = OleInitialize(None);
    }
    info!("Filee Lite {} starting; app {}", env!("CARGO_PKG_VERSION"), paths::app_dir().display());

    core::store::load();
    let settings = core::store::settings();
    ui::i18n::init(settings.get("language").and_then(|v| v.as_str()).unwrap_or("auto"));
    let palette = ui::theme::palette(&settings);

    let hwnd = create_main_window();
    MAIN_HWND.with(|h| h.set(hwnd.0 as isize));
    events::set_target(hwnd);
    ui::tray::create(hwnd);
    ui::toast::init();
    ui::toast::set_palette(&palette);
    ui::toast::set_job_actions(ui::toast::JobActions {
        cancel: Box::new(|id| {
            let _ = core::jobs::cancel(id);
        }),
        retry: Box::new(|id| {
            let _ = core::jobs::retry(id);
        }),
    });
    donut::init();
    donut::set_style(&palette, settings.get("donut").unwrap_or(&serde_json::Value::Null));
    donut::set_faces_provider(Box::new(|files| core::profiles::faces_for(files)));
    apply_settings(&settings);
    input::start();

    unsafe {
        SetTimer(Some(hwnd), TIMER_DATA, 1500, None);
        SetTimer(Some(hwnd), TIMER_ENGINES, ENGINES_CHECK_MS, None);
    }
    if !args.convert.is_empty() {
        events::post(AppEvent::Convert(args.convert.clone()));
    } else if args.settings || (!args.background && !raw.iter().any(|a| a.starts_with("--"))) {
        events::post(AppEvent::OpenSettings);
    }
    info!("Filee Lite started");

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    input::stop();
    ui::tray::remove();
    info!("Filee Lite exited");
}

/// Everything derived from settings.json; at start and whenever upstream (or the tray) changed it.
fn apply_settings(settings: &serde_json::Value) {
    input::apply(settings);
    ui::tray::set_paused(settings.get("paused").and_then(|v| v.as_bool()).unwrap_or(false));
    core::watch::apply(settings);
    core::integration::apply(settings);
}

fn create_main_window() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(main_proc),
            hInstance: instance.into(),
            lpszClassName: MAIN_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            MAIN_CLASS,
            w!("Filee Lite"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None, // a hidden top-level window (not HWND_MESSAGE): FindWindow and the tray need it
            None,
            Some(instance.into()),
            None,
        )
        .expect("main window")
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ui::tray::handle_message(hwnd, msg, wparam, lparam) {
        return LRESULT(0);
    }
    match msg {
        events::WM_APP_EVENT => {
            for event in events::drain() {
                handle_event(event);
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            if let Some(args) = single::received(lparam) {
                let parsed = parse_args(&args);
                if parsed.quit {
                    events::post(AppEvent::Quit);
                } else if !parsed.convert.is_empty() {
                    events::post(AppEvent::Convert(parsed.convert));
                } else if parsed.settings {
                    events::post(AppEvent::OpenSettings);
                }
            }
            LRESULT(1)
        }
        // Windows switched light/dark ("ImmersiveColorSet"): only the colours change, nothing else is re-applied.
        WM_SETTINGCHANGE if is_color_set_change(lparam) => {
            let settings = core::store::settings();
            let palette = ui::theme::palette(&settings);
            ui::toast::set_palette(&palette);
            donut::set_style(&palette, settings.get("donut").unwrap_or(&serde_json::Value::Null));
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_CONVERT => {
            let _ = KillTimer(Some(hwnd), TIMER_CONVERT);
            let mut files = PENDING_CONVERT.with(|p| std::mem::take(&mut *p.borrow_mut()));
            let mut seen = std::collections::HashSet::new();
            files.retain(|f| seen.insert(f.to_string_lossy().to_lowercase()));
            if !files.is_empty() {
                let (x, y) = input::last_cursor();
                donut::show_for_files(x, y, files);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_ENGINES => {
            core::engine_update::tick(core::jobs::is_busy() || core::hosted::is_running());
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_DATA => {
            if core::store::reload_if_changed() {
                handle_event(AppEvent::DataChanged);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn handle_event(event: AppEvent) {
    match event {
        AppEvent::Input(InputEvent::DragStarted { x, y }) => {
            if !paused() {
                donut::show_for_drag(x, y);
                input::set_watch_flip(true);
            }
        }
        AppEvent::Input(InputEvent::DragEnded) => donut::on_drag_gesture_ended(),
        AppEvent::Input(InputEvent::SelectionGesture { x, y }) => {
            if paused() {
                return;
            }
            let files = input::file_manager_selection();
            if files.is_empty() {
                info!("The selection shortcut found no selected files");
            } else {
                donut::show_for_files(x, y, files);
            }
        }
        AppEvent::Input(InputEvent::Flip) => donut::flip(),
        AppEvent::Donut(DonutEvent::Picked { preset_id, action, files }) => {
            input::set_watch_flip(false);
            match action {
                SliceAction::Cli => {
                    core::jobs::run_cli(&preset_id, files);
                }
                SliceAction::Hosted => core::hosted::run(&preset_id, &files),
                SliceAction::Group => {}
            }
        }
        AppEvent::Donut(DonutEvent::Closed) => input::set_watch_flip(false),
        AppEvent::Donut(DonutEvent::EditRequested { preset_id }) => core::hosted::edit_preset(&preset_id),
        AppEvent::Job(status) => {
            ui::toast::show(&status);
            if status.state != model::JobState::Running {
                core::history::record(&status);
            }
        }
        AppEvent::Convert(files) => {
            PENDING_CONVERT.with(|p| p.borrow_mut().extend(files));
            let hwnd = HWND(MAIN_HWND.with(|h| h.get()) as *mut _);
            unsafe {
                SetTimer(Some(hwnd), TIMER_CONVERT, CONVERT_DEBOUNCE_MS, None);
            }
        }
        AppEvent::DataChanged => {
            let settings = core::store::settings();
            let palette = ui::theme::palette(&settings);
            ui::toast::set_palette(&palette);
            donut::set_style(&palette, settings.get("donut").unwrap_or(&serde_json::Value::Null));
            apply_settings(&settings);
        }
        AppEvent::OpenSettings => core::hosted::open_settings(),
        AppEvent::Quit => unsafe { PostQuitMessage(0) },
    }
}

fn is_color_set_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    let text = unsafe { windows::core::PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
    text == "ImmersiveColorSet"
}

fn paused() -> bool {
    core::store::settings().get("paused").and_then(|v| v.as_bool()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_keeps_existing_files_only() {
        let exe = std::env::current_exe().unwrap();
        let args = parse_args(&[
            "--convert".into(),
            exe.to_string_lossy().into_owned(),
            r"C:\definitely\not\here.png".into(),
            "--background".into(),
        ]);
        assert!(args.background);
        assert_eq!(args.convert, vec![exe]);
    }

    #[test]
    fn convert_list_reads_one_path_per_line() {
        let dir = std::env::temp_dir().join("filee-lite-test-list");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().unwrap();
        let list = dir.join("list.txt");
        std::fs::write(&list, format!("\u{feff}{}\r\n\r\n", exe.display())).unwrap();
        let args = parse_args(&["--convert-list".into(), list.to_string_lossy().into_owned()]);
        assert_eq!(args.convert, vec![exe]);
    }
}
