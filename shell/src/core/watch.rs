//! Auto-convert folders (spec 02 §4): a port of upstream's `FolderWatcher` on top of `filee-cli convert`.
//!
//! One thread per enabled rule polls its folder every second (no change notifications: polling cannot overflow and
//! costs a directory listing). A file is converted once it has stayed unchanged for 2 s and opens exclusively; each
//! ready batch is one CLI run (per subfolder when the rule includes subfolders, so the folder structure carries into
//! the output as upstream's "keep folders from" does). Rules that rename outputs, or whose preset replaces the
//! original, run with a temporary data folder holding a presets.json with an adjusted copy of the preset. Results
//! written inside the watched tree are remembered in `.filee-watch-<hash>.json`, in upstream's format, so a restart
//! does not convert them again.

use std::collections::{HashMap, HashSet};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::core::formats::FormatTable;
use crate::core::jobs::{self, FileState, JobSpec, Outcome};
use crate::core::{store, util};
use crate::{info, warn};

/// How long a file must stay unchanged (Windows: 2 s).
const SETTLE: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_secs(1);
const DEFAULT_OUTPUT_NAME: &str = "converted";
const ORIGINALS_NAME: &str = "originals";
const TEMPORARY_EXTENSIONS: [&str; 9] = [".tmp", ".temp", ".part", ".partial", ".crdownload", ".download", ".opdownload", ".!ut", ".filepart"];
const SYSTEM_FILES: [&str; 2] = ["desktop.ini", "thumbs.db"];
/// .NET ticks (100 ns since 0001) minus FILETIME ticks (since 1601).
const TICKS_1601: u64 = 504_911_232_000_000_000;

/// One `watchFolders[]` entry of settings.json.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rule {
    pub id: String,
    pub folder: PathBuf,
    pub preset_id: String,
    pub include_subfolders: bool,
    pub output_folder: String,
    pub move_originals: bool,
    pub include: Vec<String>,
    pub file_name_pattern: String,
    pub renames: Vec<Value>,
}

impl Rule {
    /// None for a rule upstream would not run either (disabled, or folder or preset missing).
    pub fn from_value(v: &Value) -> Option<Rule> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        if v.get("enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let folder = text("folder");
        let preset_id = text("presetId");
        if folder.trim().is_empty() || preset_id.trim().is_empty() {
            return None;
        }
        Some(Rule {
            id: text("id"),
            folder: PathBuf::from(folder.trim()),
            preset_id: preset_id.trim().to_string(),
            include_subfolders: v.get("includeSubfolders").and_then(Value::as_bool).unwrap_or(false),
            output_folder: text("outputFolder"),
            move_originals: v.get("originals").and_then(Value::as_str).is_some_and(|o| o.eq_ignore_ascii_case("MoveToOriginals")),
            include: v
                .get("include")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            file_name_pattern: text("fileNamePattern"),
            renames: v.get("renames").and_then(Value::as_array).cloned().unwrap_or_default(),
        })
    }

    pub fn resolved_output(&self) -> PathBuf {
        if self.output_folder.trim().is_empty() {
            self.folder.join(DEFAULT_OUTPUT_NAME)
        } else {
            PathBuf::from(self.output_folder.trim())
        }
    }

    pub fn originals_folder(&self) -> PathBuf {
        self.folder.join(ORIGINALS_NAME)
    }

    /// `WatchFolderService`'s signature: a changed rule restarts its watcher.
    pub fn signature(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.id,
            self.folder.display(),
            self.preset_id,
            self.include_subfolders,
            self.output_folder,
            self.move_originals,
            self.include.join("\u{1}"),
            self.file_name_pattern,
            serde_json::to_string(&self.renames).unwrap_or_default()
        )
    }

    fn blank_rename(step: &Value) -> bool {
        let kind = step.get("kind").and_then(Value::as_str).unwrap_or("");
        let len = |k: &str| step.get(k).and_then(Value::as_str).map(str::len).unwrap_or(0);
        match kind {
            "Replace" | "Remove" | "Regex" => len("find") == 0,
            "Prefix" | "Suffix" => len("replace") == 0,
            _ => false,
        }
    }

    fn real_renames(&self) -> Vec<Value> {
        self.renames.iter().filter(|r| !Self::blank_rename(r)).cloned().collect()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Manager: one thread per rule, rebuilt when the rules change

struct Running {
    signature: String,
    stops: Vec<Arc<AtomicBool>>,
    /// Set by each watch thread when it has ended, plus those of older generations that had not ended yet when this
    /// one started. A rebuild waits for all of them so two generations never convert the same file.
    finished: Vec<Arc<AtomicBool>>,
}

/// The longest a rebuilt watcher waits for the previous one to leave its batch before it starts anyway.
const HANDOVER_LIMIT: Duration = Duration::from_secs(600);

/// Sets its flag when dropped, so a watch thread that panics still counts as ended.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Waits until every flag is set. False when `stop` is set or `limit` passes first.
fn wait_until_finished(flags: &[Arc<AtomicBool>], stop: &AtomicBool, limit: Duration, step: Duration) -> bool {
    let until = Instant::now() + limit;
    while !flags.iter().all(|f| f.load(Ordering::SeqCst)) {
        if stop.load(Ordering::SeqCst) || Instant::now() >= until {
            return false;
        }
        std::thread::sleep(step);
    }
    true
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Starts, stops or rebuilds the watchers to match settings.json.
pub fn apply(settings: &Value) {
    let rules: Vec<Rule> = settings
        .get("watchFolders")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Rule::from_value).collect())
        .unwrap_or_default();
    let signature = rules.iter().map(Rule::signature).collect::<Vec<_>>().join("\n");
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if running.as_ref().is_some_and(|r| r.signature == signature) || (running.is_none() && rules.is_empty()) {
        return;
    }
    // The old threads end after the batch they are in; the new ones start only once those have ended.
    let mut previous: Vec<Arc<AtomicBool>> = Vec::new();
    if let Some(old) = running.take() {
        for stop in old.stops {
            stop.store(true, Ordering::SeqCst);
        }
        previous = old.finished.into_iter().filter(|f| !f.load(Ordering::SeqCst)).collect();
    }
    let previous = Arc::new(previous);
    let mut stops = Vec::new();
    let mut finished: Vec<Arc<AtomicBool>> = previous.iter().cloned().collect();
    for rule in rules {
        let stop = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        stops.push(stop.clone());
        finished.push(ended.clone());
        info!("Watch folder {} -> preset {}", rule.folder.display(), rule.preset_id);
        let name = format!("watch-{}", rule.id);
        let (after, flag) = (previous.clone(), ended.clone());
        if let Err(e) = std::thread::Builder::new().name(name).spawn(move || {
            let _ended = SetOnDrop(flag);
            if !after.iter().all(|f| f.load(Ordering::SeqCst)) {
                if !wait_until_finished(&after, &stop, HANDOVER_LIMIT, Duration::from_millis(100)) && !stop.load(Ordering::SeqCst) {
                    warn!("Watch folder {}: the previous watcher is still busy; starting anyway", rule.folder.display());
                }
            }
            watch_loop(rule, stop)
        }) {
            warn!("Could not start a watch thread: {e}");
            ended.store(true, Ordering::SeqCst);
        }
    }
    *running = Some(Running { signature, stops, finished });
}

fn watch_loop(rule: Rule, stop: Arc<AtomicBool>) {
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let mut watcher = Watcher::new(rule.clone());
    let mut announced = false;
    while !stop.load(Ordering::SeqCst) {
        if watcher.folder_exists() {
            let now = Instant::now();
            watcher.scan(now);
            let ready = watcher.take_ready(now);
            if !ready.is_empty() && !stop.load(Ordering::SeqCst) {
                run_batch(&mut watcher, ready);
            }
        } else if !announced {
            warn!("Watch folder not found: {}", rule.folder.display());
            announced = true;
        }
        // Sleep in small steps so a rebuild takes effect quickly.
        let until = Instant::now() + TICK;
        while Instant::now() < until && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The watcher state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    len: u64,
    ticks: u64,
}

struct Candidate {
    stat: Option<Stat>,
    stable_since: Instant,
}

pub struct Watcher {
    pub rule: Rule,
    /// Versions already seen on disk (so an unchanged file is not noticed again).
    known: HashMap<String, Stat>,
    pending: HashMap<String, (PathBuf, Candidate)>,
    /// `path|length|ticks` of files already handed to a conversion.
    handled: HashSet<String>,
    results: GeneratedOutputs,
    started: bool,
    include: NameFilter,
}

impl Watcher {
    pub fn new(rule: Rule) -> Watcher {
        let results = GeneratedOutputs::load(&rule);
        let include = NameFilter::of(&rule.include);
        Watcher { rule, known: HashMap::new(), pending: HashMap::new(), handled: HashSet::new(), results, started: false, include }
    }

    pub fn folder_exists(&self) -> bool {
        self.rule.folder.is_dir()
    }

    /// Notices new and changed files. The first scan only counts files that were already there when originals are
    /// moved away after conversion (they arrived while nobody was watching); otherwise they were converted before.
    pub fn scan(&mut self, now: Instant) {
        let first = !self.started;
        self.started = true;
        let files = list_files(&self.rule.folder, self.rule.include_subfolders);
        let table = FormatTable::embedded();
        let mut seen: HashSet<String> = HashSet::new();
        for (path, stat) in files {
            let key = util::path_key(&path);
            seen.insert(key.clone());
            let Some(stat) = stat else { continue };
            if self.known.get(&key) == Some(&stat) {
                continue;
            }
            self.known.insert(key.clone(), stat);
            if first && !self.rule.move_originals {
                continue;
            }
            if should_ignore(&path, &self.rule, &self.include, table) {
                continue;
            }
            match self.pending.get_mut(&key) {
                Some((_, cand)) if cand.stat == Some(stat) => {}
                Some((_, cand)) => {
                    cand.stat = Some(stat);
                    cand.stable_since = now;
                }
                None => {
                    self.pending.insert(key, (path, Candidate { stat: Some(stat), stable_since: now }));
                }
            }
        }
        self.known.retain(|k, _| seen.contains(k));
    }

    /// The pending files that are complete: unchanged for the settle time and openable exclusively.
    pub fn take_ready(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        let mut done: Vec<String> = Vec::new();
        for (key, (path, cand)) in self.pending.iter_mut() {
            let current = stat_of(path);
            if current.is_none() || self.results.contains(path) {
                done.push(key.clone());
                continue;
            }
            if current != cand.stat {
                cand.stat = current;
                cand.stable_since = now;
            }
            if now.saturating_duration_since(cand.stable_since) < SETTLE || !can_open_exclusively(path) {
                continue;
            }
            done.push(key.clone());
            let s = current.expect("checked");
            if self.handled.insert(format!("{key}|{}|{}", s.len, s.ticks)) {
                ready.push(path.clone());
            }
        }
        for key in done {
            self.pending.remove(&key);
        }
        ready.sort_by_key(|p| util::path_key(p));
        ready
    }

    /// After a batch: remember results inside the tree, and move converted sources to `originals`.
    pub fn finish_batch(&mut self, outcome: &Outcome) {
        let rule = &self.rule;
        let out_folder = rule.resolved_output();
        let mut changed = false;
        for r in &outcome.results {
            for output in &r.outputs {
                // A dedicated output tree is excluded wholesale; only results that share the watched tree need the state file.
                if util::same_folder(&out_folder, &rule.folder) || !util::is_inside(&out_folder, &rule.folder) {
                    changed |= self.results.remember(output);
                }
                if let Some(s) = stat_of(output) {
                    self.handled.insert(format!("{}|{}|{}", util::path_key(output), s.len, s.ticks));
                }
            }
        }
        if changed {
            if let Err(e) = self.results.save() {
                warn!("Could not save the watch state for {}: {e}", rule.folder.display());
            }
        }
        if rule.move_originals {
            for r in outcome.results.iter().filter(|r| r.state == FileState::Done) {
                move_to_originals(rule, &r.source);
            }
        }
    }
}

/// The files under `folder` with their size and modification time. The numbers come from the directory listing
/// itself (`DirEntry::metadata` opens nothing on Windows), so a scan costs no file open per file per second.
fn list_files(folder: &Path, recurse: bool) -> Vec<(PathBuf, Option<Stat>)> {
    let mut out = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if recurse {
                    stack.push(entry.path());
                }
            } else if kind.is_file() {
                out.push((entry.path(), entry.metadata().ok().and_then(|m| stat_from(&m))));
            }
        }
    }
    out
}

fn stat_of(path: &Path) -> Option<Stat> {
    stat_from(&std::fs::metadata(path).ok()?)
}

fn stat_from(meta: &std::fs::Metadata) -> Option<Stat> {
    if !meta.is_file() {
        return None;
    }
    Some(Stat { len: meta.len(), ticks: util::filetime_of(meta.modified().ok()?) + TICKS_1601 })
}

fn can_open_exclusively(path: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).share_mode(0).open(path).is_ok()
}

/// `FolderWatcher.ShouldIgnore`.
pub fn should_ignore(path: &Path, rule: &Rule, include: &NameFilter, table: &FormatTable) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_lowercase();
    if name.is_empty()
        || name.starts_with("~$")
        || name.starts_with('.')
        || SYSTEM_FILES.contains(&lower.as_str())
        || TEMPORARY_EXTENSIONS.iter().any(|e| lower.ends_with(e))
    {
        return true;
    }
    let out = rule.resolved_output();
    // A dedicated output tree is excluded; never the whole watch root when the output is that folder or a parent of it.
    if !util::same_folder(&out, &rule.folder) && util::is_inside(&out, &rule.folder) && util::is_inside(path, &out) {
        return true;
    }
    if util::is_inside(path, &rule.originals_folder()) {
        return true;
    }
    if !rule.include_subfolders && !path.parent().is_some_and(|p| util::same_folder(p, &rule.folder)) {
        return true;
    }
    if table.detect(path).is_none() {
        return true;
    }
    if !include.matches(&name) {
        return true;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::windows::fs::MetadataExt;
        let attrs = meta.file_attributes();
        // Directory 0x10, hidden 0x2, system 0x4.
        if attrs & 0x16 != 0 {
            return true;
        }
    } else {
        return true;
    }
    false
}

/// Moves a converted source into the originals folder, keeping its subfolder and numbering a taken name.
pub fn move_to_originals(rule: &Rule, source: &Path) {
    if !util::is_inside(source, &rule.folder) || util::is_inside(source, &rule.originals_folder()) {
        return;
    }
    let Ok(relative) = source.strip_prefix(&rule.folder).map(Path::to_path_buf).or_else(|_| {
        // Case differences in the prefix: fall back to the part after the folder's length.
        let s = source.to_string_lossy();
        let f = rule.folder.to_string_lossy();
        let f = f.trim_end_matches(['\\', '/']);
        s.get(f.len()..).map(|r| PathBuf::from(r.trim_start_matches(['\\', '/']))).ok_or(())
    }) else {
        return;
    };
    let mut target = rule.originals_folder().join(&relative);
    if let Some(dir) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!("Could not create {}: {e}", dir.display());
            return;
        }
    }
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = target.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut n = 2;
    while target.exists() {
        target = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    if let Err(e) = std::fs::rename(source, &target) {
        warn!("Could not move {} to the originals folder: {e}", source.display());
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Running a batch

pub(crate) fn run_batch(watcher: &mut Watcher, files: Vec<PathBuf>) {
    let rule = watcher.rule.clone();
    let Some(preset) = store::find_preset(&rule.preset_id) else {
        warn!("Watch folder {}: preset '{}' not found; skipping {} file(s)", rule.folder.display(), rule.preset_id, files.len());
        return;
    };
    if matches!(preset.get("kind").and_then(Value::as_str), Some(k) if !k.eq_ignore_ascii_case("convert")) {
        warn!("Watch folder {}: preset '{}' is not a conversion; skipping {} file(s)", rule.folder.display(), rule.preset_id, files.len());
        return;
    }
    info!("Watch folder {}: converting {} file(s)", rule.folder.display(), files.len());
    let plan = plan_batches(&rule, &preset, &files);
    let temp_dir = match &plan {
        Plan::Ephemeral { preset: p, .. } => prepare_temp_data(&rule, p),
        Plan::PerFolder(_) => None,
    };
    let mut all = Outcome {
        id: 0,
        status: crate::model::JobStatus {
            id: 0,
            title: String::new(),
            preset_id: rule.preset_id.clone(),
            total: 0,
            done: 0,
            failed: 0,
            state: crate::model::JobState::Completed,
            outputs: vec![],
            errors: vec![],
        },
        results: Vec::new(),
    };
    let specs: Vec<JobSpec> = match (&plan, &temp_dir) {
        (Plan::PerFolder(groups), _) => groups
            .iter()
            .map(|(out, group)| JobSpec {
                preset_id: rule.preset_id.clone(),
                files: group.clone(),
                output_folder: Some(out.clone()),
                watch_rule_id: Some(rule.id.clone()),
                ..Default::default()
            })
            .collect(),
        (Plan::Ephemeral { id, .. }, Some(dir)) => vec![JobSpec {
            preset_id: id.clone(),
            files: files.clone(),
            data_dir: Some(dir.clone()),
            watch_rule_id: Some(rule.id.clone()),
            report_preset_id: Some(rule.preset_id.clone()),
            ..Default::default()
        }],
        (Plan::Ephemeral { .. }, None) => {
            warn!("Watch folder {}: could not prepare the temporary data folder", rule.folder.display());
            return;
        }
    };
    for spec in &specs {
        let outcome = jobs::run_blocking(spec);
        info!(
            "Watch folder {}: {:?}, {} done, {} failed",
            rule.folder.display(),
            outcome.status.state,
            outcome.status.done,
            outcome.status.failed
        );
        all.results.extend(outcome.results.iter().cloned());
        all.id = outcome.id;
    }
    if let Some(dir) = temp_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    watcher.finish_batch(&all);
}

/// How a batch runs.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// `-o <output>\<subfolder>` per source subfolder; the preset as it is.
    PerFolder(Vec<(PathBuf, Vec<PathBuf>)>),
    /// A temporary preset (id) with the rule's output settings applied, in a temporary data folder.
    Ephemeral { id: String, preset: Value },
}

const EPHEMERAL_ID: &str = "eph-watch";

pub fn plan_batches(rule: &Rule, preset: &Value, files: &[PathBuf]) -> Plan {
    let out = preset.get("output");
    let keeps_from = out.and_then(|o| o.get("keepFoldersFrom")).and_then(Value::as_str).unwrap_or("");
    let replaces = out.and_then(|o| o.get("replaceOriginal")).and_then(Value::as_bool).unwrap_or(false);
    if !rule.file_name_pattern.trim().is_empty() || !rule.real_renames().is_empty() || replaces || !keeps_from.is_empty() {
        return Plan::Ephemeral { id: EPHEMERAL_ID.into(), preset: ephemeral_preset(rule, preset) };
    }
    // Files of one source folder go to the same sub-path of the output, as "keep folders from" does.
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    let base = rule.resolved_output();
    for f in files {
        let dir = f.parent().unwrap_or(Path::new(""));
        let target = if util::is_inside(dir, &rule.folder) {
            let rel = dir.to_string_lossy().get(rule.folder.to_string_lossy().trim_end_matches(['\\', '/']).len()..).unwrap_or("").trim_start_matches(['\\', '/']).to_string();
            if rel.is_empty() { base.clone() } else { base.join(rel) }
        } else {
            base.clone()
        };
        match groups.iter_mut().find(|(o, _)| util::same_folder(o, &target)) {
            Some((_, g)) => g.push(f.clone()),
            None => groups.push((target, vec![f.clone()])),
        }
    }
    Plan::PerFolder(groups)
}

/// `WatchRule.Apply(preset)`.
pub fn ephemeral_preset(rule: &Rule, preset: &Value) -> Value {
    let mut p = preset.clone();
    if let Some(obj) = p.as_object_mut() {
        obj.insert("id".into(), json!(EPHEMERAL_ID));
        let output = obj.entry("output").or_insert_with(|| json!({}));
        if let Some(o) = output.as_object_mut() {
            o.insert("location".into(), json!("CustomFolder"));
            o.insert("customFolder".into(), json!(rule.resolved_output().to_string_lossy()));
            o.insert("keepFoldersFrom".into(), json!(rule.folder.to_string_lossy()));
            o.insert("replaceOriginal".into(), json!(false));
            if !rule.file_name_pattern.trim().is_empty() {
                o.insert("fileNamePattern".into(), json!(rule.file_name_pattern.trim()));
            }
            let renames = o.entry("renames").or_insert_with(|| json!([]));
            if let Some(list) = renames.as_array_mut() {
                list.extend(rule.real_renames());
            }
        }
    }
    p
}

/// A temporary data folder: upstream's settings, profiles and history, and a presets.json with the temporary preset
/// added. (Engines downloaded into the normal data folder are not found from there.)
fn prepare_temp_data(rule: &Rule, ephemeral: &Value) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("FileeLite").join(format!("watch-{}-{}", rule.id, util::new_job_id(1)));
    std::fs::create_dir_all(&dir).ok()?;
    for name in ["settings.json", "profiles.json"] {
        let src = crate::paths::data_dir().join(name);
        if src.exists() {
            std::fs::copy(&src, dir.join(name)).ok()?;
        }
    }
    let mut presets = store::presets();
    presets.as_array_mut()?.push(ephemeral.clone());
    std::fs::write(dir.join("presets.json"), util::to_upstream_json(&presets, false)).ok()?;
    Some(dir)
}

// ---------------------------------------------------------------------------------------------------------------
// File name filter (`FileNameFilter`): globs and /regular expressions/, case-insensitive. A regular expression
// written for .NET that the `regex` crate can't take (look-around, back-references) is skipped, as upstream skips an
// invalid one.

#[derive(Clone, Debug, Default)]
pub struct NameFilter {
    globs: Vec<Vec<char>>,
    regexes: Vec<regex::Regex>,
}

impl NameFilter {
    pub fn of(patterns: &[String]) -> NameFilter {
        let mut f = NameFilter::default();
        for p in patterns {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            if p.chars().count() > 2 && p.starts_with('/') && p.ends_with('/') {
                let body = &p[1..p.len() - 1];
                match regex::RegexBuilder::new(body).case_insensitive(true).size_limit(1 << 20).build() {
                    Ok(r) => f.regexes.push(r),
                    Err(e) => warn!("Watch-folder pattern {p} is left out: {e}"),
                }
                continue;
            }
            f.globs.push(p.to_lowercase().chars().collect());
        }
        f
    }

    /// No usable pattern means every file.
    pub fn matches(&self, name: &str) -> bool {
        if self.globs.is_empty() && self.regexes.is_empty() {
            return true;
        }
        if self.regexes.iter().any(|r| r.is_match(name)) {
            return true;
        }
        let name: Vec<char> = name.to_lowercase().chars().collect();
        self.globs.iter().any(|g| glob_match(g, &name))
    }
}

/// `*` any text, `?` one character; the whole name must match.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) && pattern[p] != '*' {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

// ---------------------------------------------------------------------------------------------------------------
// `.filee-watch-<sha256(rule id)[..16]>.json` (GeneratedWatchOutputs), compatible with upstream's

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    path: PathBuf,
    length: u64,
    ticks: u64,
    is_dir: bool,
}

pub struct GeneratedOutputs {
    folder: PathBuf,
    state_file: PathBuf,
    entries: HashMap<String, Entry>,
}

pub fn state_file_name(rule_id: &str) -> String {
    format!(".filee-watch-{}.json", util::sha256_hex_prefix(rule_id, 16))
}

impl GeneratedOutputs {
    pub fn load(rule: &Rule) -> GeneratedOutputs {
        let mut g = GeneratedOutputs {
            folder: rule.folder.clone(),
            state_file: rule.folder.join(state_file_name(&rule.id)),
            entries: HashMap::new(),
        };
        if let Some(Value::Array(list)) = util::read_json_file(&g.state_file) {
            for item in list {
                let path = item.get("Path").and_then(Value::as_str).unwrap_or("");
                let entry = Entry {
                    path: PathBuf::from(path),
                    length: item.get("Length").and_then(Value::as_u64).unwrap_or(0),
                    ticks: item.get("LastWriteTicks").and_then(Value::as_u64).unwrap_or(0),
                    is_dir: item.get("IsDirectory").and_then(Value::as_bool).unwrap_or(false),
                };
                if g.is_current(&entry) {
                    g.entries.insert(util::path_key(&entry.path), entry);
                }
            }
        }
        g
    }

    /// A copied or edited state file cannot exclude the root itself or files outside the folder.
    fn is_current(&self, e: &Entry) -> bool {
        if e.path.as_os_str().is_empty() || !util::is_inside(&e.path, &self.folder) {
            return false;
        }
        if e.is_dir {
            return e.path.is_dir();
        }
        stat_of(&e.path).is_some_and(|s| s.len == e.length && s.ticks == e.ticks)
    }

    pub fn remember(&mut self, path: &Path) -> bool {
        if !util::is_inside(path, &self.folder) {
            return false;
        }
        let entry = if path.is_dir() {
            Entry { path: path.to_path_buf(), length: 0, ticks: 0, is_dir: true }
        } else {
            let Some(s) = stat_of(path) else { return false };
            Entry { path: path.to_path_buf(), length: s.len, ticks: s.ticks, is_dir: false }
        };
        self.entries.insert(util::path_key(path), entry);
        true
    }

    pub fn contains(&self, path: &Path) -> bool {
        if let Some(e) = self.entries.get(&util::path_key(path)) {
            if self.is_current(e) {
                return true;
            }
        }
        self.entries.values().any(|e| e.is_dir && util::is_inside(path, &e.path))
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let stale: Vec<String> = self.entries.iter().filter(|(_, e)| !self.is_current(e)).map(|(k, _)| k.clone()).collect();
        for k in stale {
            self.entries.remove(&k);
        }
        let list: Vec<Value> = self
            .entries
            .values()
            .map(|e| json!({"Path": e.path.to_string_lossy(), "Length": e.length, "LastWriteTicks": e.ticks, "IsDirectory": e.is_dir}))
            .collect();
        let text = util::to_compact_upstream_json(&Value::Array(list));
        set_hidden(&self.state_file, false);
        let result = util::atomic_write(&self.state_file, text.as_bytes());
        set_hidden(&self.state_file, true);
        result
    }
}

fn set_hidden(path: &Path, hidden: bool) {
    use windows::Win32::Storage::FileSystem::{GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_FLAGS_AND_ATTRIBUTES};
    let wide = util::wide(&path.to_string_lossy());
    unsafe {
        let attrs = GetFileAttributesW(windows::core::PCWSTR(wide.as_ptr()));
        if attrs == u32::MAX {
            return;
        }
        let new = if hidden { attrs | FILE_ATTRIBUTE_HIDDEN.0 } else { attrs & !FILE_ATTRIBUTE_HIDDEN.0 };
        let _ = SetFileAttributesW(windows::core::PCWSTR(wide.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(new));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::store::test_support::scratch;

    fn rule(folder: &Path) -> Rule {
        Rule { id: "1a2b3c4d".into(), folder: folder.to_path_buf(), preset_id: "to-png".into(), ..Default::default() }
    }

    fn rule_json() -> Value {
        serde_json::from_str(
            r#"{"id":"r1","enabled":true,"folder":"C:\\Inbox","presetId":"to-jpg","includeSubfolders":true,
                "outputFolder":"","originals":"MoveToOriginals","include":["*.heic","scan_*"],
                "fileNamePattern":"{name}_x","renames":[{"kind":"Uppercase","find":"","replace":""},{"kind":"Replace","find":"","replace":"x"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_rebuilt_watcher_waits_for_the_old_one_to_end() {
        let flags = vec![Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true))];
        let stop = AtomicBool::new(false);
        // still busy: gives up at the limit
        assert!(!wait_until_finished(&flags, &stop, Duration::from_millis(60), Duration::from_millis(10)));
        // stopped while waiting: gives up at once
        let stopped = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!wait_until_finished(&flags, &stopped, Duration::from_secs(30), Duration::from_millis(10)));
        assert!(started.elapsed() < Duration::from_secs(5));
        // the old watcher ends (a panic counts too, via the drop guard)
        let old = flags[0].clone();
        let t = std::thread::spawn(move || {
            let _guard = SetOnDrop(old);
            std::thread::sleep(Duration::from_millis(80));
        });
        assert!(wait_until_finished(&flags, &stop, Duration::from_secs(30), Duration::from_millis(10)));
        t.join().unwrap();
        assert!(wait_until_finished(&[], &stop, Duration::ZERO, Duration::from_millis(1)));
    }

    #[test]
    fn the_scan_listing_carries_sizes_and_times_without_opening_files() {
        let dir = scratch("watch-list-files");
        std::fs::write(dir.join("a.png"), b"12345").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("b.png"), b"1").unwrap();
        let top = list_files(&dir, false);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1.map(|s| s.len), Some(5));
        assert_eq!(top[0].1, stat_of(&top[0].0));
        let all = list_files(&dir, true);
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|(_, stat)| stat.is_some()));
    }

    #[test]
    fn rules_are_read_like_upstream() {
        let r = Rule::from_value(&rule_json()).unwrap();
        assert_eq!(r.folder, PathBuf::from(r"C:\Inbox"));
        assert!(r.include_subfolders && r.move_originals);
        assert_eq!(r.resolved_output(), PathBuf::from(r"C:\Inbox\converted"));
        assert_eq!(r.originals_folder(), PathBuf::from(r"C:\Inbox\originals"));
        // Blank steps (a Replace without text) are left out; Uppercase needs no text.
        assert_eq!(r.real_renames().len(), 1);
        let mut disabled = rule_json();
        disabled["enabled"] = json!(false);
        assert!(Rule::from_value(&disabled).is_none());
        let mut no_preset = rule_json();
        no_preset["presetId"] = json!(" ");
        assert!(Rule::from_value(&no_preset).is_none());
        let mut other = r.clone();
        other.include_subfolders = false;
        assert_ne!(r.signature(), other.signature());
    }

    #[test]
    fn name_filter_globs() {
        let f = NameFilter::of(&["*.HEIC".into(), "scan_??".into()]);
        assert!(f.matches("IMG_001.heic"));
        assert!(f.matches("scan_01"));
        assert!(!f.matches("scan_001"));
        assert!(!f.matches("photo.png"));
        assert!(NameFilter::of(&[]).matches("anything"));
        // A regular expression between slashes, as upstream: unanchored, case ignored.
        let re = NameFilter::of(&["/^IMG_\\d+/".into()]);
        assert!(re.matches("img_0042.heic"));
        assert!(!re.matches("whatever.png"));
        let mixed = NameFilter::of(&["/^IMG_\\d+/".into(), "*.pdf".into()]);
        assert!(mixed.matches("report.PDF") && mixed.matches("IMG_1.png") && !mixed.matches("a.png"));
        // Only patterns that can't be used: nothing usable, so everything passes (upstream's "all invalid").
        assert!(NameFilter::of(&["/(?<=a)b/".into()]).matches("whatever.png"));
        assert!(glob_match(&"a*b*c".chars().collect::<Vec<_>>(), &"aXXbYYc".chars().collect::<Vec<_>>()));
        assert!(!glob_match(&"a*b*c".chars().collect::<Vec<_>>(), &"aXXbYY".chars().collect::<Vec<_>>()));
        assert!(glob_match(&"*".chars().collect::<Vec<_>>(), &[]));
    }

    #[test]
    fn ignore_rules() {
        let dir = scratch("watch-ignore");
        let table = FormatTable::embedded();
        let r = rule(&dir);
        let all = NameFilter::of(&[]);
        let touch = |rel: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
            p
        };
        assert!(!should_ignore(&touch("a.png"), &r, &all, table));
        assert!(should_ignore(&touch("~$a.docx"), &r, &all, table));
        assert!(should_ignore(&touch(".hidden.png"), &r, &all, table));
        assert!(should_ignore(&touch("thumbs.db"), &r, &all, table));
        assert!(should_ignore(&touch("a.png.crdownload"), &r, &all, table));
        assert!(should_ignore(&touch("b.part"), &r, &all, table));
        assert!(should_ignore(&touch("unknown.zzz"), &r, &all, table));
        // Output and originals trees; subfolders only when asked for.
        assert!(should_ignore(&touch("converted/c.png"), &r, &all, table));
        assert!(should_ignore(&touch("originals/d.png"), &r, &all, table));
        assert!(should_ignore(&touch("sub/e.png"), &r, &all, table));
        let mut deep = r.clone();
        deep.include_subfolders = true;
        assert!(!should_ignore(&dir.join("sub/e.png"), &deep, &all, table));
        // Output = the watched folder itself: nothing under it is excluded wholesale (issue #234).
        let mut same = r.clone();
        same.output_folder = dir.to_string_lossy().into_owned();
        assert!(!should_ignore(&dir.join("a.png"), &same, &all, table));
        // Include patterns.
        assert!(should_ignore(&dir.join("a.png"), &r, &NameFilter::of(&["*.heic".into()]), table));
    }

    #[test]
    fn settle_and_exclusive_open() {
        let dir = scratch("watch-settle");
        let file = dir.join("a.png");
        std::fs::write(&file, b"one").unwrap();
        let mut w = Watcher::new(rule(&dir));
        let t0 = Instant::now();
        // First scan of a Keep rule: existing files are not candidates.
        w.scan(t0);
        assert!(w.take_ready(t0 + Duration::from_secs(5)).is_empty());
        // A new file appears: not ready at once, ready after the settle time.
        let new = dir.join("b.png");
        std::fs::write(&new, b"hello").unwrap();
        w.scan(t0);
        assert!(w.take_ready(t0 + Duration::from_secs(1)).is_empty());
        // Still being written (its size changes): the clock starts over.
        std::fs::write(&new, b"hello world").unwrap();
        w.scan(t0 + Duration::from_secs(1));
        assert!(w.take_ready(t0 + Duration::from_secs(2)).is_empty());
        // Held open by a writer: not ready even when settled.
        let holder = std::fs::OpenOptions::new().write(true).share_mode(0).open(&new).unwrap();
        assert!(w.take_ready(t0 + Duration::from_secs(10)).is_empty());
        drop(holder);
        let ready = w.take_ready(t0 + Duration::from_secs(10));
        assert_eq!(ready, vec![new.clone()]);
        // The same version is not converted twice.
        w.scan(t0 + Duration::from_secs(11));
        assert!(w.take_ready(t0 + Duration::from_secs(20)).is_empty());
        // A changed version is.
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&new, b"changed again!").unwrap();
        w.scan(t0 + Duration::from_secs(21));
        assert_eq!(w.take_ready(t0 + Duration::from_secs(30)), vec![new]);
    }

    #[test]
    fn existing_files_are_converted_when_originals_are_moved() {
        let dir = scratch("watch-existing");
        std::fs::write(dir.join("old.png"), b"x").unwrap();
        let mut r = rule(&dir);
        r.move_originals = true;
        let mut w = Watcher::new(r);
        let t0 = Instant::now();
        w.scan(t0);
        assert_eq!(w.take_ready(t0 + Duration::from_secs(5)), vec![dir.join("old.png")]);
    }

    #[test]
    fn originals_are_moved_with_their_subfolder_and_numbered() {
        let dir = scratch("watch-originals");
        let mut r = rule(&dir);
        r.include_subfolders = true;
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let a = dir.join("sub").join("a.png");
        std::fs::write(&a, b"1").unwrap();
        move_to_originals(&r, &a);
        assert!(!a.exists());
        assert!(dir.join("originals").join("sub").join("a.png").exists());
        std::fs::write(&a, b"2").unwrap();
        move_to_originals(&r, &a);
        assert!(dir.join("originals").join("sub").join("a (2).png").exists());
        // Outside the folder, or already in originals: untouched.
        let outside = scratch("watch-originals-outside").join("z.png");
        std::fs::write(&outside, b"z").unwrap();
        move_to_originals(&r, &outside);
        assert!(outside.exists());
        let inside = dir.join("originals").join("sub").join("a.png");
        move_to_originals(&r, &inside);
        assert!(inside.exists());
    }

    #[test]
    fn state_file_is_upstream_compatible() {
        assert_eq!(state_file_name("cli"), ".filee-watch-99bb88401742848e.json");
        let dir = scratch("watch-state");
        let mut r = rule(&dir);
        r.output_folder = dir.to_string_lossy().into_owned(); // output into the watched folder itself
        let out = dir.join("conv.jpg");
        std::fs::write(&out, b"123").unwrap();
        let mut g = GeneratedOutputs::load(&r);
        assert!(g.remember(&out));
        assert!(!g.remember(&scratch("watch-state-out").join("x.jpg"))); // outside the tree
        g.save().unwrap();
        let file = dir.join(state_file_name(&r.id));
        let text = std::fs::read_to_string(&file).unwrap();
        // PascalCase, compact, the .NET ticks.
        assert!(text.starts_with("[{\"Path\":"), "{text}");
        assert!(text.contains("\"Length\":3") && text.contains("\"LastWriteTicks\":") && text.contains("\"IsDirectory\":false"));
        {
            use std::os::windows::fs::MetadataExt;
            assert!(std::fs::metadata(&file).unwrap().file_attributes() & 0x2 != 0, "hidden");
        }
        // Reloaded (a restart): still recognised; a changed file is not.
        let g2 = GeneratedOutputs::load(&r);
        assert!(g2.contains(&out));
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&out, b"123456").unwrap();
        let g3 = GeneratedOutputs::load(&r);
        assert!(!g3.contains(&out));
        // A hand-edited state file cannot exclude the root or files elsewhere.
        std::fs::write(&file, format!("[{{\"Path\":{:?},\"Length\":0,\"LastWriteTicks\":0,\"IsDirectory\":true}}]", dir.to_string_lossy())).unwrap();
        assert!(!GeneratedOutputs::load(&r).contains(&dir.join("anything.jpg")));
        // Saving again after the hidden file exists works (replaces it).
        let mut g4 = GeneratedOutputs::load(&r);
        std::fs::write(&out, b"1234567").unwrap();
        assert!(g4.remember(&out));
        g4.save().unwrap();
    }

    #[test]
    fn batches_are_grouped_by_source_subfolder() {
        let mut r = rule(Path::new(r"C:\Inbox"));
        r.include_subfolders = true;
        let preset = json!({"id":"to-png","output":{"keepFoldersFrom":"","replaceOriginal":false}});
        let files = vec![
            PathBuf::from(r"C:\Inbox\a.png"),
            PathBuf::from(r"C:\Inbox\2025\b.png"),
            PathBuf::from(r"C:\Inbox\2025\c.png"),
            PathBuf::from(r"c:\inbox\2025\jan\d.png"),
        ];
        let Plan::PerFolder(groups) = plan_batches(&r, &preset, &files) else { panic!("expected per-folder") };
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], (PathBuf::from(r"C:\Inbox\converted"), vec![files[0].clone()]));
        assert_eq!(groups[1].0, PathBuf::from(r"C:\Inbox\converted\2025"));
        assert_eq!(groups[1].1.len(), 2);
        assert_eq!(groups[2].0, PathBuf::from(r"C:\Inbox\converted\2025\jan"));
    }

    #[test]
    fn rules_with_name_rules_or_replace_original_need_a_temporary_preset() {
        let mut r = rule(Path::new(r"C:\Inbox"));
        r.output_folder = r"D:\Out".into();
        r.file_name_pattern = "{name}_x".into();
        r.renames = vec![json!({"kind":"Uppercase"}), json!({"kind":"Prefix","find":"","replace":""})];
        let preset = json!({"id":"to-png","output":{"location":"Default","renames":[{"kind":"Lowercase"}],"replaceOriginal":true,"fileNamePattern":"{name}"}});
        let Plan::Ephemeral { id, preset: p } = plan_batches(&r, &preset, &[PathBuf::from(r"C:\Inbox\a.png")]) else {
            panic!("expected a temporary preset")
        };
        assert_eq!(id, "eph-watch");
        assert_eq!(p["id"], "eph-watch");
        let o = &p["output"];
        assert_eq!(o["location"], "CustomFolder");
        assert_eq!(o["customFolder"], r"D:\Out");
        assert_eq!(o["keepFoldersFrom"], r"C:\Inbox");
        assert_eq!(o["replaceOriginal"], false);
        assert_eq!(o["fileNamePattern"], "{name}_x");
        // The preset's own steps first, then the rule's non-blank ones.
        assert_eq!(o["renames"], json!([{"kind":"Lowercase"}, {"kind":"Uppercase"}]));
        // The preset's other fields are carried over untouched.
        assert_eq!(preset["output"]["replaceOriginal"], true);
        // A preset that replaces its originals needs it even without name rules.
        let plain = rule(Path::new(r"C:\Inbox"));
        assert!(matches!(plan_batches(&plain, &preset, &[]), Plan::Ephemeral { .. }));
    }
}
