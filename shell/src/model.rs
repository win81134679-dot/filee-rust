//! Types shared by the modules. Each module owns its own files; this one is the contract between them.
//! Upstream data (settings.json, presets.json, profiles.json, history.json) is read by `core::store`.

use std::path::PathBuf;

/// One slice of a donut face, already decided for the files at hand (see `core::profiles`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slice {
    /// Preset id (`to-png`, `tool-crop`, a group's id …).
    pub preset_id: String,
    /// Text drawn on the slice, translated.
    pub label: String,
    /// Short target text under the label (upstream draws the target format, e.g. "PNG"), may be empty.
    pub sub_label: String,
    /// False draws the slice dimmed; `reason` is the hint shown for it.
    pub enabled: bool,
    pub reason: Option<String>,
    /// A group: `children` is its ring (at most 6, never groups).
    pub is_group: bool,
    pub children: Vec<Slice>,
    /// How the slice runs when picked.
    pub action: SliceAction,
}

/// What picking a slice does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SliceAction {
    /// `filee-cli convert --preset <id>` in the background; the toast shows it.
    #[default]
    Cli,
    /// Needs a window of upstream Filee: tools, merge order, "ask every time" → `Filee.exe --hosted --run=<id>`.
    Hosted,
    /// A group: opens its ring.
    Group,
}

/// The two faces of the donut for one set of files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Faces {
    pub profile_id: String,
    /// Profile name for the hub, translated.
    pub profile_label: String,
    pub front: Vec<Slice>,
    pub back: Vec<Slice>,
}

/// Colours and sizes for everything Filee Lite draws (donut, toast). Filled by `ui::theme` from settings.json's
/// `theme` and upstream's palette; ARGB as 0xAARRGGBB.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub accent: u32,
    pub accent_text: u32,
    pub surface: u32,
    pub surface_alt: u32,
    pub text: u32,
    pub text_muted: u32,
    pub border: u32,
    pub danger: u32,
    pub success: u32,
    pub corner_radius: f32,
    /// Font families in fallback order (DirectWrite names).
    pub fonts: Vec<String>,
    /// Reduce animations (Windows "Show animations" off, or upstream's setting).
    pub reduce_motion: bool,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            dark: false,
            accent: 0xFF7F77DD,
            accent_text: 0xFFFFFFFF,
            surface: 0xFFFFFFFF,
            surface_alt: 0xFFF3F2F8,
            text: 0xFF1F1D2B,
            text_muted: 0xFF6B6880,
            border: 0x1F000000,
            danger: 0xFFD9534F,
            success: 0xFF2E9E6A,
            corner_radius: 16.0,
            fonts: vec!["Microsoft JhengHei UI".into(), "Segoe UI".into()],
            reduce_motion: false,
        }
    }
}

/// Events from the input hook (`input`), delivered on the UI thread.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// A drag gesture passed its threshold over the file manager: show the donut before files are known.
    DragStarted { x: i32, y: i32 },
    /// The mouse button of that drag went up (the OLE drop may or may not follow).
    DragEnded,
    /// A key chord / hold gesture: read the file manager's selection and show the donut in click mode.
    SelectionGesture { x: i32, y: i32 },
    /// The flip key or a wheel turn while a drag donut shows files.
    Flip,
}

/// Events from the donut window.
#[derive(Clone, Debug, PartialEq)]
pub enum DonutEvent {
    /// A slice was dropped on / clicked: run it.
    Picked { preset_id: String, action: SliceAction, files: Vec<PathBuf> },
    /// Closed without picking.
    Closed,
    /// Right-click / ✎ on a slice: edit it in upstream's preset editor.
    EditRequested { preset_id: String },
}

/// A conversion job the shell runs (CLI) and the toast shows.
#[derive(Clone, Debug, PartialEq)]
pub struct JobStatus {
    pub id: u64,
    pub title: String,
    pub preset_id: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub state: JobState,
    pub outputs: Vec<PathBuf>,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Completed,
    CompletedWithErrors,
    Failed,
    Cancelled,
}
