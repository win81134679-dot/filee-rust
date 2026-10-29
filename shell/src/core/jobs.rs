//! Conversions through upstream's `filee-cli.exe convert … --preset <id>` (spec 02 §3).
//!
//! Progress: the CLI's human mode prints one line per finished file (`ok`/`skip`/`stop`/`fail`) and a final
//! `Converted N of M file(s)[, K failed].`; its `--json` mode prints once, at the end. The human mode is the only
//! progress there is, so it is what runs here; outputs and failures come from those lines.
//! Every state change is an `AppEvent::Job`. When a job ends its per-file results wait in a side table for
//! `history::record`, which needs more than `JobStatus` carries.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::core::{store, util};
use crate::events::{self, AppEvent};
use crate::model::{JobState, JobStatus};
use crate::ui::i18n;
use crate::{info, warn};

/// Command lines stay below this many characters per CLI call (the OS limit is 32 767).
const MAX_COMMAND_CHARS: usize = 28_000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static RUNNING: Mutex<Vec<(u64, Arc<Handle>)>> = Mutex::new(Vec::new());
static DETAILS: Mutex<Vec<(u64, Detail)>> = Mutex::new(Vec::new());
static RETRY: Mutex<Vec<(u64, JobSpec, Vec<PathBuf>)>> = Mutex::new(Vec::new());

/// True while a conversion runs (the weekly engine update waits for it).
pub fn is_busy() -> bool {
    !lock(&RUNNING).is_empty()
}

/// Runs the failed files of finished job `id` again with the same preset and options (the toast's Retry).
/// Returns the new job's id, or None when that job had no failures (or is too old to remember).
pub fn retry(id: u64) -> Option<u64> {
    let (spec, failed) = {
        let mut retry = lock(&RETRY);
        let pos = retry.iter().position(|(i, _, _)| *i == id)?;
        let (_, spec, failed) = retry.remove(pos);
        (spec, failed)
    };
    let failed: Vec<PathBuf> = failed.into_iter().filter(|p| p.exists()).collect();
    if failed.is_empty() {
        return None;
    }
    info!("Retrying {} failed file(s) of job {id}", failed.len());
    Some(run(JobSpec { files: failed, ..spec }))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Handle {
    child: Mutex<Option<RunningChild>>,
    cancelled: AtomicBool,
}

/// A Win32 job object that kills every process in it when closed. The CLI starts ffmpeg, soffice and the like;
/// killing only the CLI would leave them running (and holding its output pipes open).
struct KillOnClose(isize);

impl KillOnClose {
    /// A job holding `child`; None when Windows refuses (the child is then killed on its own).
    fn containing(child: &Child) -> Option<KillOnClose> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let job = CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set.is_err() || AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())).is_err() {
                let _ = CloseHandle(job);
                return None;
            }
            Some(KillOnClose(job.0 as isize))
        }
    }

    fn terminate(&self) {
        use windows::Win32::Foundation::HANDLE;
        let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(HANDLE(self.0 as *mut std::ffi::c_void), 1) };
    }
}

impl Drop for KillOnClose {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        let _ = unsafe { CloseHandle(HANDLE(self.0 as *mut std::ffi::c_void)) };
    }
}

/// The CLI process of the running chunk and the job that holds it and its descendants.
struct RunningChild {
    child: Child,
    job: Option<KillOnClose>,
}

impl RunningChild {
    fn new(child: Child) -> RunningChild {
        let job = KillOnClose::containing(&child);
        RunningChild { child, job }
    }

    /// The whole process tree, or just the CLI when no job could be made.
    fn kill(&mut self) {
        match &self.job {
            Some(job) => job.terminate(),
            None => {
                let _ = self.child.kill();
            }
        }
    }
}

/// Puts a freshly started chunk where `cancel` can find it. A cancel that landed before this point (after the
/// caller's flag check) found nothing to kill, so the flag is checked again under the same lock `cancel` takes.
fn adopt(handle: &Handle, mut running: RunningChild) {
    let mut slot = lock(&handle.child);
    if handle.cancelled.load(Ordering::SeqCst) {
        running.kill();
    }
    *slot = Some(running);
}

/// What one run of the CLI is asked to do.
#[derive(Clone, Debug, Default)]
pub struct JobSpec {
    pub preset_id: String,
    pub files: Vec<PathBuf>,
    /// `-o <folder>`.
    pub output_folder: Option<PathBuf>,
    /// `FILEE_DATA_DIR` for this run (a temporary presets.json with a rule's own output settings).
    pub data_dir: Option<PathBuf>,
    /// Set for jobs a watch folder started; ends up in the history entry.
    pub watch_rule_id: Option<String>,
    /// The preset id shown and recorded when it differs from the one the CLI runs (a temporary copy of a preset).
    pub report_preset_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Done,
    Skipped,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileResult {
    pub source: PathBuf,
    pub state: FileState,
    pub outputs: Vec<PathBuf>,
    pub error: Option<String>,
}

/// Everything about a finished job that `JobStatus` does not carry.
#[derive(Clone, Debug)]
pub struct Detail {
    pub sources: Vec<PathBuf>,
    pub results: Vec<FileResult>,
    pub watch_rule_id: Option<String>,
}

/// The result of a finished run.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub id: u64,
    pub status: JobStatus,
    pub results: Vec<FileResult>,
}

/// The per-file results of a finished job, once (taken by `history::record`).
pub fn take_detail(id: u64) -> Option<Detail> {
    let mut details = lock(&DETAILS);
    let pos = details.iter().position(|(i, _)| *i == id)?;
    Some(details.remove(pos).1)
}

/// `filee-cli convert <files> --preset <id>` in the background; returns the job id at once.
pub fn run_cli(preset_id: &str, files: Vec<PathBuf>) -> u64 {
    run(JobSpec { preset_id: preset_id.to_string(), files, ..Default::default() })
}

/// Same, with options.
pub fn run(spec: JobSpec) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let spawned = std::thread::Builder::new().name(format!("job-{id}")).spawn(move || {
        execute(id, &spec);
    });
    if let Err(e) = spawned {
        warn!("Could not start the job thread: {e}");
    }
    id
}

/// Runs a job on the calling thread until it has ended.
pub fn run_blocking(spec: &JobSpec) -> Outcome {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    execute(id, spec)
}

/// Ends a running job: the CLI is killed (it cannot be asked nicely without a console). Returns what stays behind,
/// or None when no such job is running.
pub fn cancel(id: u64) -> Option<String> {
    let handle = lock(&RUNNING).iter().find(|(i, _)| *i == id).map(|(_, h)| h.clone())?;
    handle.cancelled.store(true, Ordering::SeqCst);
    if let Some(running) = lock(&handle.child).as_mut() {
        running.kill();
    }
    let note = "Cancelled. Files already converted keep their results; the file being converted may leave a partial \
                output next to it, and upstream's temporary folder in %TEMP%\\Filee is cleaned the next time Filee starts."
        .to_string();
    info!("Job {id} cancelled");
    Some(note)
}

// ---------------------------------------------------------------------------------------------------------------

fn preset_display_name(preset: &Value) -> String {
    let name = preset.get("name").and_then(Value::as_str).unwrap_or("");
    if !name.trim().is_empty() {
        return name.to_string();
    }
    match preset.get("nameKey").and_then(Value::as_str) {
        Some(key) => i18n::t(key),
        None => preset.get("targetFormat").and_then(Value::as_str).unwrap_or("").to_uppercase(),
    }
}

/// A preset's name in the UI language (also used for history entries).
pub fn display_name_of(preset_id: &str) -> String {
    match store::find_preset(preset_id) {
        Some(p) => preset_display_name(&p),
        None => preset_id.to_string(),
    }
}

fn post(status: &JobStatus) {
    events::post(AppEvent::Job(status.clone()));
}

fn execute(id: u64, spec: &JobSpec) -> Outcome {
    let handle = Arc::new(Handle { child: Mutex::new(None), cancelled: AtomicBool::new(false) });
    lock(&RUNNING).push((id, handle.clone()));
    let report_id = spec.report_preset_id.clone().unwrap_or_else(|| spec.preset_id.clone());
    let mut status = JobStatus {
        id,
        title: display_name_of(&report_id),
        preset_id: report_id,
        total: spec.files.len(),
        done: 0,
        failed: 0,
        state: JobState::Running,
        outputs: Vec::new(),
        errors: Vec::new(),
    };
    post(&status);

    let json_mode = store::find_preset(&spec.preset_id).or_else(|| spec_preset(spec)).is_some_and(|p| makes_one_result(&p));
    let mut results: Vec<FileResult> = Vec::new();
    let mut exit_code: Option<i32> = None;
    let mut stats_total: Option<usize> = None;
    let mut stderr_text = String::new();
    let mut spawn_error: Option<String> = None;

    for chunk in chunks(&spec.files) {
        if handle.cancelled.load(Ordering::SeqCst) {
            break;
        }
        let mut cmd = util::hidden_command(&crate::paths::filee_cli());
        cmd.arg("convert").args(&chunk).arg("--preset").arg(&spec.preset_id);
        if json_mode {
            cmd.arg("--json");
        }
        if let Some(out) = &spec.output_folder {
            cmd.arg("-o").arg(out);
        }
        if let Some(dir) = &spec.data_dir {
            cmd.env("FILEE_DATA_DIR", dir);
        }
        // A fixed working folder, so the CLI's path shortening (relative to the cwd) never applies to user files.
        cmd.current_dir(crate::paths::lite_dir());
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                spawn_error = Some(format!("Could not start filee-cli: {e}"));
                break;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        adopt(&handle, RunningChild::new(child));
        let err_thread = stderr.map(|mut s| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                String::from_utf8_lossy(&buf).into_owned()
            })
        });

        let results_before = results.len();
        let mut saw_stats = false;
        if json_mode {
            // A merge writes one result for all files, which the human mode does not print: read the JSON at the end.
            let mut buf = Vec::new();
            if let Some(mut out) = stdout {
                let _ = out.read_to_end(&mut buf);
            }
            let text = String::from_utf8_lossy(&buf);
            for result in parse_json_results(&text).unwrap_or_default() {
                match result.state {
                    FileState::Done | FileState::Skipped => status.done += 1,
                    FileState::Failed => status.failed += 1,
                    FileState::Cancelled => {}
                }
                status.outputs.extend(result.outputs.iter().cloned());
                if let Some(e) = &result.error {
                    status.errors.push(e.clone());
                }
                results.push(result);
            }
            if !results.is_empty() {
                stats_total = Some(results.len());
            }
        } else if let Some(out) = stdout {
            let mut remaining: Vec<PathBuf> = chunk.clone();
            for line in BufReader::new(out).split(b'\n') {
                let Ok(bytes) = line else { break };
                let text = String::from_utf8_lossy(&bytes);
                let text = text.trim_end_matches('\r');
                if let Some(total) = parse_stats_line(text) {
                    saw_stats = true;
                    stats_total = Some(stats_total.unwrap_or(0) + total);
                    continue;
                }
                if let Some(result) = parse_result_line(text, &remaining) {
                    remaining.retain(|r| util::path_key(r) != util::path_key(&result.source));
                    match result.state {
                        FileState::Done | FileState::Skipped => status.done += 1,
                        FileState::Failed => status.failed += 1,
                        FileState::Cancelled => {}
                    }
                    status.outputs.extend(result.outputs.iter().cloned());
                    if let Some(e) = &result.error {
                        status.errors.push(e.clone());
                    }
                    results.push(result);
                    post(&status);
                }
            }
        }
        let mut chunk_stderr = String::new();
        if let Some(t) = err_thread {
            chunk_stderr = t.join().unwrap_or_default();
            stderr_text.push_str(&chunk_stderr);
        }
        let running = lock(&handle.child).take();
        let mut chunk_exit: Option<i32> = None;
        if let Some(mut running) = running {
            chunk_exit = running.child.wait().ok().and_then(|s| s.code());
            exit_code = chunk_exit;
            // `running.job` closes here and takes any leftover descendants with it.
        }
        // A CLI that died or stopped early never reported some files: they did not convert, so they failed.
        if !handle.cancelled.load(Ordering::SeqCst)
            && chunk_incomplete(chunk_exit, saw_stats || json_mode, results.len() > results_before)
        {
            let why = unreported_reason(chunk_exit, &chunk_stderr);
            for source in unreported_sources(&chunk, &results[results_before..]) {
                status.failed += 1;
                status.errors.push(format!("{}: {why}", source.display()));
                results.push(FileResult { source, state: FileState::Failed, outputs: Vec::new(), error: Some(why.clone()) });
            }
            post(&status);
        }
    }

    let cancelled = handle.cancelled.load(Ordering::SeqCst);
    let mut errors = status.errors.clone();
    if let Some(e) = spawn_error {
        errors.push(e);
    }
    if results.is_empty() {
        for line in stderr_text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            errors.push(line.trim_start_matches("filee: ").to_string());
        }
    }
    if let Some(total) = stats_total {
        status.total = total.max(results.len());
    }
    // Sources never reported (the run was cut short) stay "cancelled".
    if cancelled {
        for f in &spec.files {
            if !results.iter().any(|r| util::path_key(&r.source) == util::path_key(f)) {
                results.push(FileResult { source: f.clone(), state: FileState::Cancelled, outputs: Vec::new(), error: None });
            }
        }
    }
    // Success without a single parsed line: trust the exit code (the CLI printed something unexpected).
    if results.is_empty() && !cancelled && exit_code == Some(0) {
        status.done = spec.files.len();
    }
    status.state = final_state(cancelled, status.done, status.failed, exit_code, results.is_empty());
    status.errors = errors;

    lock(&RUNNING).retain(|(i, _)| *i != id);
    {
        // For the toast's Retry: the sources that failed, kept after history has taken the detail.
        let failed: Vec<PathBuf> =
            results.iter().filter(|r| r.state == FileState::Failed).map(|r| r.source.clone()).collect();
        if !failed.is_empty() {
            let mut retry = lock(&RETRY);
            retry.push((id, spec.clone(), failed));
            while retry.len() > 20 {
                retry.remove(0);
            }
        }
        let mut details = lock(&DETAILS);
        details.push((
            id,
            Detail { sources: spec.files.clone(), results: results.clone(), watch_rule_id: spec.watch_rule_id.clone() },
        ));
        while details.len() > 50 {
            details.remove(0);
        }
    }
    info!(
        "Job {id} ({}) ended: {:?}, {} done, {} failed, exit {:?}",
        spec.preset_id, status.state, status.done, status.failed, exit_code
    );
    post(&status);
    Outcome { id, status, results }
}

/// The preset of a job whose id is not in the library (a temporary copy in the job's own data folder).
fn spec_preset(spec: &JobSpec) -> Option<Value> {
    let dir = spec.data_dir.as_ref()?;
    let presets = util::read_json_file(&dir.join("presets.json"))?;
    presets.as_array()?.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(spec.preset_id.as_str())).cloned()
}

/// Presets that turn all the files of a job into one result (merge PDF / images / videos / audio, one archive,
/// multi-page TIFF): the CLI's human mode lists no output for them.
pub fn makes_one_result(preset: &Value) -> bool {
    let flag = |a: &str, b: &str| preset.get(a).and_then(|o| o.get(b)).and_then(Value::as_bool).unwrap_or(false);
    let target = preset.get("targetFormat").and_then(Value::as_str).unwrap_or("");
    (flag("pdf", "mergeIntoSingle") && target == "pdf")
        || flag("media", "joinIntoOne")
        || flag("media", "slideshow")
        || flag("image", "joinIntoOne")
        || flag("image", "multiPageTiff")
        || flag("archive", "combineIntoOne")
}

/// The CLI's `--json` output: `files[]` with source, state, outputs, error.
pub fn parse_json_results(text: &str) -> Option<Vec<FileResult>> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let files = v.get("files")?.as_array()?;
    Some(
        files
            .iter()
            .map(|f| FileResult {
                source: PathBuf::from(f.get("source").and_then(Value::as_str).unwrap_or("")),
                state: match f.get("state").and_then(Value::as_str).unwrap_or("") {
                    "done" => FileState::Done,
                    "skipped" => FileState::Skipped,
                    "cancelled" => FileState::Cancelled,
                    _ => FileState::Failed,
                },
                outputs: f
                    .get("outputs")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|o| o.as_str().map(PathBuf::from)).collect())
                    .unwrap_or_default(),
                error: f.get("error").and_then(Value::as_str).map(String::from),
            })
            .collect(),
    )
}

/// Did the CLI end a chunk without finishing it? A non-zero (or missing) exit code says so. A clean exit does too
/// when the human mode printed some results but never the final `Converted N of M` line (`summary_seen` is passed as
/// true for the JSON mode, which prints once at the end and has no such line).
pub fn chunk_incomplete(exit_code: Option<i32>, summary_seen: bool, printed_results: bool) -> bool {
    exit_code != Some(0) || (printed_results && !summary_seen)
}

fn unreported_reason(exit_code: Option<i32>, stderr: &str) -> String {
    let mut why = match exit_code {
        Some(code) => format!("filee-cli stopped (exit code {code}) before reporting this file"),
        None => "filee-cli ended abnormally before reporting this file".to_string(),
    };
    if let Some(line) = stderr.lines().map(str::trim).find(|l| !l.is_empty()) {
        why.push_str(": ");
        why.push_str(line.trim_start_matches("filee: "));
    }
    why
}

/// The sources of a chunk that none of its results account for. A folder counts as accounted for when some result
/// lies inside it (the CLI expands folders and reports their files).
pub fn unreported_sources(chunk: &[PathBuf], results: &[FileResult]) -> Vec<PathBuf> {
    let reported: Vec<String> = results.iter().map(|r| util::path_key(&r.source)).collect();
    chunk
        .iter()
        .filter(|source| {
            let key = util::path_key(source);
            let folder_prefix = format!("{}\\", key.trim_end_matches(['\\', '/']));
            !reported.iter().any(|r| *r == key || r.starts_with(&folder_prefix))
        })
        .cloned()
        .collect()
}

/// `JobQueue`'s rule: cancelled, or all files done/skipped → Completed, some done → with errors, else Failed.
pub fn final_state(cancelled: bool, done: usize, failed: usize, exit_code: Option<i32>, no_results: bool) -> JobState {
    if cancelled {
        JobState::Cancelled
    } else if no_results {
        // Nothing reported: only a clean exit counts as success.
        if exit_code == Some(0) {
            JobState::Completed
        } else {
            JobState::Failed
        }
    } else if failed == 0 {
        JobState::Completed
    } else if done > 0 {
        JobState::CompletedWithErrors
    } else {
        JobState::Failed
    }
}

/// Splits the files so no CLI call exceeds the command-line limit (a merge preset over more files than fit loses
/// its single result; with that many files there is no better way).
pub fn chunks(files: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let mut out: Vec<Vec<PathBuf>> = Vec::new();
    let mut size = 0usize;
    for f in files {
        let len = f.as_os_str().len() + 3;
        if out.is_empty() || size + len > MAX_COMMAND_CHARS {
            out.push(Vec::new());
            size = 0;
        }
        out.last_mut().map(|c| c.push(f.clone()));
        size += len;
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    out
}

/// `Converted N of M file(s)[, K failed].` → M.
pub fn parse_stats_line(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("Converted ")?;
    let (_, after) = rest.split_once(" of ")?;
    let (total, tail) = after.split_once(" file")?;
    if !tail.starts_with("(s)") {
        return None;
    }
    total.trim().parse().ok()
}

/// One result line of the CLI's human mode (`Cli.cs` `Describe`), attributed to one of `remaining` when the printed
/// path matches (case-insensitive), else to the path as printed.
pub fn parse_result_line(line: &str, remaining: &[PathBuf]) -> Option<FileResult> {
    let (status, state) = ["ok", "skip", "stop", "fail"].iter().find_map(|s| {
        let rest = line.strip_prefix(s)?;
        rest.starts_with(' ').then(|| {
            let st = match *s {
                "ok" => FileState::Done,
                "skip" => FileState::Skipped,
                "stop" => FileState::Cancelled,
                _ => FileState::Failed,
            };
            (rest.trim_start(), st)
        })
    })?;
    let lower = status.to_lowercase();
    let matched = remaining
        .iter()
        .find(|p| {
            let key = p.to_string_lossy().to_lowercase();
            lower.starts_with(&key) && matches!(lower[key.len()..].chars().next(), None | Some(' ') | Some(':'))
        })
        .cloned();
    let (source, after) = match &matched {
        Some(p) => (p.clone(), status.get(p.to_string_lossy().len()..).unwrap_or("").to_string()),
        None => {
            let cut = match state {
                FileState::Done => status.find(" -> "),
                FileState::Failed => status.find(": "),
                _ => status.rfind(" ("),
            };
            let (src, rest) = status.split_at(cut.unwrap_or(status.len()));
            (PathBuf::from(src), rest.to_string())
        }
    };
    let mut outputs = Vec::new();
    let mut error = None;
    match state {
        FileState::Done => {
            if let Some(list) = after.trim_start().strip_prefix("-> ") {
                outputs = split_outputs(list);
            }
        }
        FileState::Failed => {
            error = Some(after.trim_start().trim_start_matches(':').trim().to_string());
        }
        _ => {}
    }
    Some(FileResult { source, state, outputs, error })
}

/// `a.png, b.png (notice)` → the paths. Output paths may contain ", ", so pieces that do not look like the start of an
/// absolute path are glued back; a trailing ` (notice)` after the last file name is dropped.
pub fn split_outputs(list: &str) -> Vec<PathBuf> {
    let mut text = list.trim().to_string();
    if text.ends_with(')') {
        // The notice follows the last file name, which has an extension: cut at the first " (" after one.
        let cut = text.match_indices(" (").map(|(i, _)| i).find(|&i| {
            let ext = text[..i].rsplit('.').next().unwrap_or("");
            text[..i].contains('.') && !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
        if let Some(i) = cut {
            text.truncate(i);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for piece in text.split(", ") {
        let b = piece.as_bytes();
        let absolute = piece.starts_with("\\\\") || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\');
        if absolute || out.is_empty() {
            out.push(piece.to_string());
        } else if let Some(last) = out.last_mut() {
            last.push_str(", ");
            last.push_str(piece);
        }
    }
    out.into_iter().map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srcs(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn parses_ok_lines() {
        let remaining = srcs(&[r"C:\in\a.png", r"C:\in\b c.png"]);
        let r = parse_result_line(r"ok    C:\in\a.png -> C:\out\a.webp", &remaining).unwrap();
        assert_eq!(r.state, FileState::Done);
        assert_eq!(r.source, PathBuf::from(r"C:\in\a.png"));
        assert_eq!(r.outputs, srcs(&[r"C:\out\a.webp"]));
        // Several outputs, a source with a space, and a notice.
        let r = parse_result_line(r"ok    C:\in\b c.png -> C:\out\b c_p1.png, C:\out\b c_p2.png (the PDF has a password)", &remaining).unwrap();
        assert_eq!(r.source, PathBuf::from(r"C:\in\b c.png"));
        assert_eq!(r.outputs, srcs(&[r"C:\out\b c_p1.png", r"C:\out\b c_p2.png"]));
        // A merge: later files are done with nothing of their own.
        let r = parse_result_line(r"ok    C:\in\b.png", &srcs(&[r"C:\in\b.png"])).unwrap();
        assert_eq!((r.state, r.outputs.len()), (FileState::Done, 0));
    }

    #[test]
    fn parses_other_lines() {
        let remaining = srcs(&[r"C:\in\a.png", r"C:\in\bad.png"]);
        let r = parse_result_line(r"skip  C:\in\a.png (the output file already exists)", &remaining).unwrap();
        assert_eq!((r.state, r.source), (FileState::Skipped, PathBuf::from(r"C:\in\a.png")));
        let r = parse_result_line(r"fail  C:\in\bad.png: The engine reported an error (improper image header `x')", &remaining).unwrap();
        assert_eq!(r.state, FileState::Failed);
        assert_eq!(r.error.as_deref(), Some("The engine reported an error (improper image header `x')"));
        let r = parse_result_line(r"stop  C:\in\a.png (cancelled)", &remaining).unwrap();
        assert_eq!(r.state, FileState::Cancelled);
        // A path the job did not list (a folder was expanded): taken as printed.
        let r = parse_result_line(r"ok    C:\in\sub\x.png -> C:\out\x.jpg", &remaining).unwrap();
        assert_eq!(r.source, PathBuf::from(r"C:\in\sub\x.png"));
        let r = parse_result_line(r"fail  C:\in\sub\y.png: boom", &remaining).unwrap();
        assert_eq!((r.source, r.error.as_deref()), (PathBuf::from(r"C:\in\sub\y.png"), Some("boom")));
        // Not result lines.
        assert!(parse_result_line("Converted 2 of 2 file(s).", &remaining).is_none());
        assert!(parse_result_line("okay then", &remaining).is_none());
        assert!(parse_result_line("", &remaining).is_none());
    }

    #[test]
    fn parses_json_results_and_knows_merge_presets() {
        let text = r#"{"preset":"Merge PDF","target":"pdf","converted":2,"skipped":0,"failed":0,"files":[
            {"source":"C:\\in\\a.png","state":"done","outputs":["C:\\out\\a_merged.pdf"]},
            {"source":"C:\\in\\b.png","state":"done","outputs":[]},
            {"source":"C:\\in\\c.png","state":"failed","outputs":[],"error":"boom"},
            {"source":"C:\\in\\d.png","state":"skipped","outputs":[]}]}"#;
        let r = parse_json_results(text).unwrap();
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].outputs, srcs(&[r"C:\out\a_merged.pdf"]));
        assert_eq!((r[2].state, r[2].error.as_deref()), (FileState::Failed, Some("boom")));
        assert_eq!(r[3].state, FileState::Skipped);
        assert!(parse_json_results("not json").is_none());
        let p = |t: &str| serde_json::from_str::<Value>(t).unwrap();
        assert!(makes_one_result(&p(r#"{"targetFormat":"pdf","pdf":{"mergeIntoSingle":true}}"#)));
        assert!(makes_one_result(&p(r#"{"targetFormat":"mp4","media":{"joinIntoOne":true}}"#)));
        assert!(makes_one_result(&p(r#"{"targetFormat":"zip","archive":{"combineIntoOne":true}}"#)));
        assert!(!makes_one_result(&p(r#"{"targetFormat":"png","pdf":{"mergeIntoSingle":true}}"#)));
        assert!(!makes_one_result(&p(r#"{"targetFormat":"jpg","image":{"joinIntoOne":false}}"#)));
    }

    #[test]
    fn parses_the_final_line() {
        assert_eq!(parse_stats_line("Converted 6 of 6 file(s)."), Some(6));
        assert_eq!(parse_stats_line("Converted 0 of 1 file(s), 1 failed."), Some(1));
        assert_eq!(parse_stats_line("ok  x"), None);
    }

    #[test]
    fn output_lists_with_commas_and_notices() {
        assert_eq!(split_outputs(r"C:\o\a, b.png"), srcs(&[r"C:\o\a, b.png"]));
        assert_eq!(split_outputs(r"C:\o\a.png (missing fonts: Arial)"), srcs(&[r"C:\o\a.png"]));
        assert_eq!(split_outputs(r"C:\o\a (2).png"), srcs(&[r"C:\o\a (2).png"]));
        assert_eq!(split_outputs(r"\\nas\share\a.png, \\nas\share\b.png"), srcs(&[r"\\nas\share\a.png", r"\\nas\share\b.png"]));
    }

    #[test]
    fn state_rules() {
        assert_eq!(final_state(false, 3, 0, Some(0), false), JobState::Completed);
        assert_eq!(final_state(false, 2, 1, Some(1), false), JobState::CompletedWithErrors);
        assert_eq!(final_state(false, 0, 2, Some(1), false), JobState::Failed);
        assert_eq!(final_state(true, 1, 0, None, false), JobState::Cancelled);
        assert_eq!(final_state(false, 0, 0, Some(2), true), JobState::Failed);
        assert_eq!(final_state(false, 0, 0, Some(0), true), JobState::Completed);
    }

    #[test]
    fn chunking_respects_the_command_line_limit() {
        let files: Vec<PathBuf> = (0..3000).map(|i| PathBuf::from(format!(r"C:\folder\with\a\fairly\long\path\file-{i}.png"))).collect();
        let parts = chunks(&files);
        assert!(parts.len() > 1);
        assert_eq!(parts.iter().map(Vec::len).sum::<usize>(), 3000);
        for p in &parts {
            assert!(p.iter().map(|f| f.as_os_str().len() + 3).sum::<usize>() <= MAX_COMMAND_CHARS);
        }
        assert_eq!(chunks(&[]).len(), 1);
        assert_eq!(chunks(&files[..3]).len(), 1);
    }

    fn result(path: &str, state: FileState) -> FileResult {
        FileResult { source: PathBuf::from(path), state, outputs: Vec::new(), error: None }
    }

    #[test]
    fn files_the_cli_never_reported_are_found() {
        let chunk = srcs(&[r"C:\in\a.png", r"C:\in\b.png", r"C:\in\dir"]);
        let results = vec![result(r"c:\IN\A.png", FileState::Done), result(r"C:\in\dir\x.png", FileState::Done)];
        // a.png matches ignoring case, dir is covered by a file inside it, b.png was never reported
        assert_eq!(unreported_sources(&chunk, &results), srcs(&[r"C:\in\b.png"]));
        assert_eq!(unreported_sources(&chunk, &[]).len(), 3);
        // a sibling that merely starts with the same letters does not cover a folder
        assert_eq!(unreported_sources(&srcs(&[r"C:\in\dir"]), &[result(r"C:\in\dir2\x.png", FileState::Done)]).len(), 1);
    }

    #[test]
    fn a_chunk_is_incomplete_on_a_bad_exit_or_a_missing_summary() {
        assert!(chunk_incomplete(Some(1), true, true));
        assert!(chunk_incomplete(None, true, true)); // killed or never waited for
        assert!(chunk_incomplete(Some(0), false, true)); // output ended before the final line
        assert!(!chunk_incomplete(Some(0), true, true));
        assert!(!chunk_incomplete(Some(0), false, false)); // silent success: the exit code is trusted
        assert!(chunk_incomplete(Some(2), false, false));
    }

    #[test]
    fn the_unreported_reason_names_the_exit_code_and_first_stderr_line() {
        assert_eq!(
            unreported_reason(Some(3), "\nfilee: no engine\nmore"),
            "filee-cli stopped (exit code 3) before reporting this file: no engine"
        );
        assert_eq!(unreported_reason(None, ""), "filee-cli ended abnormally before reporting this file");
    }

    fn sleeper() -> Child {
        std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("ping")
    }

    #[test]
    fn cancel_before_the_child_is_stored_still_kills_it() {
        let handle = Handle { child: Mutex::new(None), cancelled: AtomicBool::new(false) };
        handle.cancelled.store(true, Ordering::SeqCst); // cancel landed after the flag check, before adopt
        adopt(&handle, RunningChild::new(sleeper()));
        let mut running = lock(&handle.child).take().expect("stored");
        let started = std::time::Instant::now();
        assert!(running.child.wait().is_ok());
        assert!(started.elapsed().as_secs() < 10, "the child was killed, not waited out");
    }

    #[test]
    fn cancel_terminates_the_whole_job() {
        let handle = Handle { child: Mutex::new(None), cancelled: AtomicBool::new(false) };
        adopt(&handle, RunningChild::new(sleeper()));
        assert!(lock(&handle.child).as_ref().is_some_and(|r| r.job.is_some()), "a job object holds the child");
        lock(&handle.child).as_mut().unwrap().kill();
        let mut running = lock(&handle.child).take().unwrap();
        let started = std::time::Instant::now();
        assert!(running.child.wait().is_ok());
        assert!(started.elapsed().as_secs() < 10);
    }

    #[test]
    fn cancel_of_unknown_job_is_none() {
        assert!(cancel(u64::MAX).is_none());
        assert!(take_detail(u64::MAX).is_none());
    }
}
