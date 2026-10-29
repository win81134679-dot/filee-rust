//! What upstream can convert: the format table (`filee-cli formats`) and, per source format, which targets work now
//! and which only with optional engines (`filee-cli formats <id>`). Spec 01 §5.4.
//!
//! The format list ships with Filee Rust (`formats_default.txt`, the output of v2.2.0) so file types are known from
//! the first drag. The per-source target sets are built in a background thread (one CLI call per source format,
//! below-normal priority), cached in `%LOCALAPPDATA%\FileeLite\formats-<version>.json`, and until a source format's
//! entry exists every route is treated as possible.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock, RwLock};

use serde_json::{json, Value};

use crate::core::util;
use crate::{info, warn};

const DEFAULT_LIST: &str = include_str!("formats_default.txt");

#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub id: String,
    pub display: String,
    /// Extensions without the dot, lower case.
    pub exts: Vec<String>,
    pub category: String,
    pub writable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FormatTable {
    pub formats: Vec<Format>,
    by_id: HashMap<String, usize>,
    by_ext: HashMap<String, usize>,
    /// Extensions with a dot in them (`tar.gz`), longest first.
    compound: Vec<String>,
}

impl FormatTable {
    pub fn parse(text: &str) -> FormatTable {
        let formats = parse_formats_list(text);
        let mut table = FormatTable { formats, ..Default::default() };
        for (i, f) in table.formats.iter().enumerate() {
            table.by_id.entry(f.id.clone()).or_insert(i);
            for e in &f.exts {
                table.by_ext.entry(e.clone()).or_insert(i);
                if e.contains('.') {
                    table.compound.push(e.clone());
                }
            }
        }
        table.compound.sort_by_key(|e| std::cmp::Reverse(e.len()));
        table.compound.dedup();
        table
    }

    pub fn embedded() -> &'static FormatTable {
        static TABLE: OnceLock<FormatTable> = OnceLock::new();
        TABLE.get_or_init(|| FormatTable::parse(DEFAULT_LIST))
    }

    pub fn by_id(&self, id: &str) -> Option<&Format> {
        self.by_id.get(id).map(|i| &self.formats[*i])
    }

    pub fn by_ext(&self, ext: &str) -> Option<&Format> {
        self.by_ext.get(&ext.trim_start_matches('.').to_lowercase()).map(|i| &self.formats[*i])
    }

    /// `FormatRegistry.ExtensionOf`: the extension without the dot, a known two-part one (`tar.gz`) as a whole.
    pub fn extension_of(&self, path: &Path) -> String {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        for compound in &self.compound {
            if name.len() > compound.len() + 1 && name.ends_with(&format!(".{compound}")) {
                return compound.clone();
            }
        }
        match name.rfind('.') {
            Some(i) => name[i + 1..].to_string(),
            None => String::new(),
        }
    }

    /// `FormatRegistry.Detect`: the format of a file by its extension, or None.
    pub fn detect(&self, path: &Path) -> Option<&Format> {
        let ext = self.extension_of(path);
        if ext.is_empty() {
            None
        } else {
            self.by_ext(&ext)
        }
    }
}

/// Parses the no-argument output of `filee-cli formats`:
/// `Image:` headers and `  png        PNG  .png .png_large` lines (`(read only)` after the name).
pub fn parse_formats_list(text: &str) -> Vec<Format> {
    let mut out = Vec::new();
    let mut category = String::new();
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with("Run '") {
            continue;
        }
        if !line.starts_with(' ') {
            if let Some(name) = line.strip_suffix(':') {
                category = name.to_string();
            }
            continue;
        }
        let body = line.trim_start();
        let Some(split) = body.find(char::is_whitespace) else { continue };
        let id = &body[..split];
        let rest = body[split..].trim_start();
        let (name_part, ext_part) = match rest.find("  .") {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let (display, writable) = match name_part.trim().strip_suffix("(read only)") {
            Some(d) => (d.trim().to_string(), false),
            None => (name_part.trim().to_string(), true),
        };
        let exts = ext_part
            .split_whitespace()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        out.push(Format { id: id.to_string(), display, exts, category: category.clone(), writable });
    }
    out
}

/// Targets of one source format.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Targets {
    /// Writable with the engines installed now.
    pub now: HashSet<String>,
    /// Only after installing optional engines.
    pub later: HashSet<String>,
}

/// Parses `filee-cli formats <id>`:
/// ```text
/// PNG (png, png_large) converts to:
///   jpg, webp, …
/// With optional engines (install them in Filee, Settings → Engines):
///   idA, idB
/// ```
pub fn parse_targets(text: &str) -> Targets {
    #[derive(PartialEq)]
    enum Part {
        None,
        Now,
        Later,
    }
    let mut part = Part::None;
    let mut t = Targets::default();
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim_end();
        if line.contains("converts to:") {
            part = Part::Now;
        } else if line.starts_with("With optional engines") {
            part = Part::Later;
        } else if line.starts_with(' ') && !line.trim().starts_with('(') {
            let set = match part {
                Part::Now => &mut t.now,
                Part::Later => &mut t.later,
                Part::None => continue,
            };
            for id in line.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                set.insert(id.to_string());
            }
        }
    }
    t
}

/// How a conversion from one format to another stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Possible with the installed engines.
    Now,
    /// Possible once an optional engine is installed.
    Later,
    /// No engine does it.
    None,
    /// Nothing is known yet for this source (the table is still being built): treated as possible.
    Unknown,
}

/// Archive formats upstream's ArchiveConverter writes (`ArchiveTargets`); it extracts to a folder from the same sources.
const ARCHIVE_TARGETS: [&str; 6] = ["zip", "7z", "tar", "tgz", "tbz2", "txz"];

/// Which routes exist, per source format id.
#[derive(Clone, Debug, Default)]
pub struct Routes {
    pub per_source: HashMap<String, Targets>,
}

impl Routes {
    pub fn route(&self, table: &FormatTable, from: &str, to: &str) -> Route {
        if to == "folder" {
            // "Extract" (target folder) is not in the CLI's list: ArchiveConverter reads an archive into a folder
            // exactly when it can repack it, so the best route into one of its archive targets stands in for it.
            let mut best = Route::None;
            for target in ARCHIVE_TARGETS.iter().filter(|t| **t != from) {
                best = match (best, self.route(table, from, target)) {
                    (Route::Now, _) | (_, Route::Now) => Route::Now,
                    (Route::Unknown, _) | (_, Route::Unknown) => Route::Unknown,
                    (Route::Later, _) | (_, Route::Later) => Route::Later,
                    _ => Route::None,
                };
            }
            return best;
        }
        if from == to {
            // The CLI's list leaves out the source itself; same-format work needs the format to be writable.
            return match table.by_id(from) {
                Some(f) if f.writable => Route::Now,
                Some(_) => Route::None,
                None => Route::Unknown,
            };
        }
        match self.per_source.get(from) {
            None => Route::Unknown,
            Some(t) if t.now.contains(to) => Route::Now,
            Some(t) if t.later.contains(to) => Route::Later,
            Some(_) => Route::None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Cache file and background builder

pub fn cache_to_json(version: &str, list_text: &str, routes: &Routes, complete: bool) -> Value {
    let mut targets = serde_json::Map::new();
    let mut keys: Vec<&String> = routes.per_source.keys().collect();
    keys.sort();
    for k in keys {
        let t = &routes.per_source[k];
        let sorted = |s: &HashSet<String>| {
            let mut v: Vec<&String> = s.iter().collect();
            v.sort();
            json!(v)
        };
        targets.insert(k.clone(), json!({ "now": sorted(&t.now), "later": sorted(&t.later) }));
    }
    json!({ "version": version, "complete": complete, "list": list_text, "targets": targets })
}

/// Cache file back into (list text, routes, complete). None when it is not a cache of this version.
pub fn cache_from_json(version: &str, v: &Value) -> Option<(String, Routes, bool)> {
    if v.get("version")?.as_str()? != version {
        return None;
    }
    let list = v.get("list")?.as_str()?.to_string();
    let mut routes = Routes::default();
    for (src, t) in v.get("targets")?.as_object()? {
        let set = |k: &str| -> HashSet<String> {
            t.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        routes.per_source.insert(src.clone(), Targets { now: set("now"), later: set("later") });
    }
    Some((list, routes, v.get("complete").and_then(Value::as_bool).unwrap_or(false)))
}

struct Live {
    table: FormatTable,
    routes: Routes,
}

fn live() -> &'static RwLock<Live> {
    static LIVE: OnceLock<RwLock<Live>> = OnceLock::new();
    LIVE.get_or_init(|| RwLock::new(Live { table: FormatTable::embedded().clone(), routes: Routes::default() }))
}

static REQUESTED: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Runs `f` with the current table and routes.
pub fn with<R>(f: impl FnOnce(&FormatTable, &Routes) -> R) -> R {
    let guard = live().read().unwrap_or_else(|e| e.into_inner());
    f(&guard.table, &guard.routes)
}

/// Asks the builder to do this source format next (a drag brought a file of that type).
pub fn request(source_id: &str) {
    let known = with(|_, r| r.per_source.contains_key(source_id));
    if !known {
        let mut q = REQUESTED.lock().unwrap_or_else(|e| e.into_inner());
        if !q.iter().any(|s| s == source_id) {
            q.push_back(source_id.to_string());
        }
    }
}

/// How many source formats have their target sets so far, and whether the build finished.
#[allow(dead_code)] // for a status line
pub fn progress() -> (usize, bool) {
    let n = with(|_, r| r.per_source.len());
    (n, BUILD_DONE.load(std::sync::atomic::Ordering::SeqCst))
}

static BUILD_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn cache_path(version: &str) -> PathBuf {
    crate::paths::lite_dir().join(format!("formats-{version}.json"))
}

fn run_cli(args: &[&str]) -> Option<String> {
    let output = util::hidden_command(&crate::paths::filee_cli())
        .args(args)
        .current_dir(crate::paths::lite_dir())
        .creation_flags_extra()
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

trait CommandExtra {
    fn creation_flags_extra(&mut self) -> &mut Self;
}

impl CommandExtra for std::process::Command {
    fn creation_flags_extra(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(util::CREATE_NO_WINDOW | util::BELOW_NORMAL)
    }
}

/// Starts the background build once.
pub fn start_background() {
    static START: Once = Once::new();
    START.call_once(|| {
        if !crate::paths::filee_cli().exists() {
            warn!("filee-cli.exe not found at {}; availability checks stay optimistic", crate::paths::filee_cli().display());
            return;
        }
        let _ = std::thread::Builder::new().name("formats-build".into()).spawn(build);
    });
}

fn build() {
    let Some(version_text) = run_cli(&["version"]) else {
        warn!("filee-cli version failed; no formats table");
        return;
    };
    let version = version_text.trim().trim_start_matches("filee").trim().to_string();
    let path = cache_path(&version);
    let mut list_text = DEFAULT_LIST.to_string();
    let mut routes = Routes::default();
    let mut complete = false;
    if let Some((list, cached, done)) = util::read_json_file(&path).and_then(|v| cache_from_json(&version, &v)) {
        list_text = list;
        routes = cached;
        complete = done;
        info!("Formats cache for {version}: {} source formats{}", routes.per_source.len(), if done { "" } else { " (partial)" });
    }
    if !complete {
        if let Some(fresh) = run_cli(&["formats"]) {
            if !parse_formats_list(&fresh).is_empty() {
                list_text = fresh;
            }
        }
    }
    publish(&list_text, &routes);
    if complete {
        BUILD_DONE.store(true, std::sync::atomic::Ordering::SeqCst);
        return;
    }

    let table = FormatTable::parse(&list_text);
    let mut order: Vec<String> = Vec::new();
    for category in ["Image", "Pdf", "Document", "Spreadsheet", "Presentation", "Video", "Audio"] {
        order.extend(table.formats.iter().filter(|f| f.category == category).map(|f| f.id.clone()));
    }
    order.extend(table.formats.iter().map(|f| f.id.clone()));
    let mut done_count = 0;
    // Sources whose `filee-cli formats <id>` failed this run: not cached (they stay unknown, so nothing is dimmed on
    // a guess), skipped by the sweep, and asked again only when a drag requests them or at the next start.
    let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let started = std::time::Instant::now();
    loop {
        let next = {
            let requested = REQUESTED.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
            requested.or_else(|| order.iter().find(|id| !routes.per_source.contains_key(*id) && !failed.contains(*id)).cloned())
        };
        let Some(id) = next else { break };
        let id_for_cli = id.clone();
        if routes.per_source.contains_key(&id) {
            continue;
        }
        record_targets(&mut routes, &mut failed, id, run_cli(&["formats", &id_for_cli]));
        done_count += 1;
        if done_count % 20 == 0 {
            publish(&list_text, &routes);
            save(&path, &version, &list_text, &routes, false);
        }
    }
    publish(&list_text, &routes);
    // Only a sweep without failures is complete; otherwise the next start asks for the missing ones again.
    let all_done = failed.is_empty();
    save(&path, &version, &list_text, &routes, all_done);
    if all_done {
        BUILD_DONE.store(true, std::sync::atomic::Ordering::SeqCst);
    } else {
        warn!("Formats table: `filee-cli formats` failed for {} source format(s); they stay unknown until the next start", failed.len());
    }
    info!("Formats table built: {} source formats in {:.0} s", routes.per_source.len(), started.elapsed().as_secs_f32());
}

/// Stores the outcome of one `filee-cli formats <id>` run. A failed run (`None`) is remembered in `failed` but not
/// stored: an empty entry would be cached as the complete answer and dim every slice for that format.
fn record_targets(routes: &mut Routes, failed: &mut std::collections::HashSet<String>, id: String, output: Option<String>) {
    match output {
        Some(text) => {
            failed.remove(&id);
            routes.per_source.insert(id, parse_targets(&text));
        }
        None => {
            failed.insert(id);
        }
    }
}

fn publish(list_text: &str, routes: &Routes) {
    let mut guard = live().write().unwrap_or_else(|e| e.into_inner());
    guard.table = FormatTable::parse(list_text);
    guard.routes = routes.clone();
}

fn save(path: &Path, version: &str, list_text: &str, routes: &Routes, complete: bool) {
    let text = serde_json::to_string(&cache_to_json(version, list_text, routes, complete)).unwrap_or_default();
    if let Err(e) = util::atomic_write(path, text.as_bytes()) {
        warn!("Could not write the formats cache: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_OUTPUT: &str = "PNG (png, png_large) converts to:\n  jpg, webp, tiff, bmp, gif, ico, avif, heic, jxl, jp2, psd, pdf, docx, mp4, dwg, dxf\nWith optional engines (install them in Filee, Settings \u{2192} Engines):\n  epub, mobi\n";

    #[test]
    fn a_failed_targets_query_is_not_cached_as_empty() {
        let mut routes = Routes::default();
        let mut failed = std::collections::HashSet::new();
        record_targets(&mut routes, &mut failed, "png".into(), None);
        assert!(!routes.per_source.contains_key("png"), "a failure leaves the format unknown");
        assert!(failed.contains("png"));
        // a later success replaces the failure
        record_targets(&mut routes, &mut failed, "png".into(), Some(PNG_OUTPUT.to_string()));
        assert_eq!(routes.per_source["png"], parse_targets(PNG_OUTPUT));
        assert!(failed.is_empty());
    }

    #[test]
    fn parses_the_formats_list() {
        let t = FormatTable::embedded();
        let png = t.by_id("png").unwrap();
        assert_eq!(png.display, "PNG");
        assert_eq!(png.category, "Image");
        assert!(png.writable);
        assert_eq!(png.exts, ["png", "png_large"]);
        let raw = t.by_id("raw").unwrap();
        assert!(!raw.writable);
        assert!(raw.exts.contains(&"dng".to_string()));
        assert_eq!(t.by_id("livp").unwrap().display, "Live Photo (LIVP)");
        assert_eq!(t.by_id("h265").unwrap().display, "H.265");
        assert_eq!(t.by_id("jxl").unwrap().display, "JPEG XL");
        assert_eq!(t.by_id("mp4").unwrap().category, "Video");
        assert!(t.formats.len() > 250, "{}", t.formats.len());
    }

    #[test]
    fn extension_detection_including_compound() {
        let t = FormatTable::embedded();
        assert_eq!(t.extension_of(Path::new(r"C:\a\b.TAR.GZ")), "tar.gz");
        assert_eq!(t.extension_of(Path::new(r"C:\a\b.tgz")), "tgz");
        assert_eq!(t.extension_of(Path::new(r"C:\a\book.kepub.epub")), "kepub.epub");
        assert_eq!(t.extension_of(Path::new(r"C:\a\noext")), "");
        assert_eq!(t.extension_of(Path::new(r"C:\a\x.tar.gz")), "tar.gz");
        assert_eq!(t.detect(Path::new("scan.JPEG")).unwrap().id, "jpg");
        assert_eq!(t.detect(Path::new("a.tar.gz")).unwrap().id, "tgz");
        assert_eq!(t.detect(Path::new("notes.log")).unwrap().id, "txt");
        assert!(t.detect(Path::new("thing.unknownext")).is_none());
        // A bare ".tar.gz" name is not a compound extension (needs a name before it).
        assert_eq!(t.extension_of(Path::new(".gz")), "gz");
    }

    #[test]
    fn parses_targets_of_one_source() {
        let t = parse_targets(PNG_OUTPUT);
        assert!(t.now.contains("jpg") && t.now.contains("dxf") && t.now.len() == 16);
        assert_eq!(t.later.len(), 2);
        assert!(t.later.contains("epub"));
        let none = parse_targets("DICOM (dcm) converts to:\n  (nothing with the engines installed now)\n");
        assert!(none.now.is_empty() && none.later.is_empty());
        let only_later = parse_targets(
            "X (x) converts to:\n  (nothing with the engines installed now)\nWith optional engines (a):\n  pdf\n",
        );
        assert!(only_later.now.is_empty() && only_later.later.contains("pdf"));
    }

    #[test]
    fn route_lookup() {
        let table = FormatTable::embedded();
        let mut routes = Routes::default();
        assert_eq!(routes.route(table, "png", "jpg"), Route::Unknown);
        routes.per_source.insert("png".into(), parse_targets(PNG_OUTPUT));
        assert_eq!(routes.route(table, "png", "jpg"), Route::Now);
        assert_eq!(routes.route(table, "png", "epub"), Route::Later);
        assert_eq!(routes.route(table, "png", "xcf"), Route::None);
        // Same format: writable or not.
        assert_eq!(routes.route(table, "png", "png"), Route::Now);
        assert_eq!(routes.route(table, "raw", "raw"), Route::None);
    }

    #[test]
    fn extracting_to_a_folder_follows_the_archive_routes() {
        // `filee-cli formats zip` (2.2.0) leaves "folder" out; the donut's "Extract" must still be usable.
        let table = FormatTable::embedded();
        let mut routes = Routes::default();
        assert_eq!(routes.route(table, "zip", "folder"), Route::Unknown);
        routes.per_source.insert("zip".into(), parse_targets("ZIP (zip, wbpz) converts to:\n  7z, tar, tgz, tbz2, txz\n"));
        assert_eq!(routes.route(table, "zip", "folder"), Route::Now);
        routes.per_source.insert("iso".into(), parse_targets("ISO (iso) converts to:\n  (nothing with the engines installed now)\nWith optional engines (7-Zip):\n  zip, 7z\n"));
        assert_eq!(routes.route(table, "iso", "folder"), Route::Later);
        routes.per_source.insert("png".into(), parse_targets(PNG_OUTPUT));
        assert_eq!(routes.route(table, "png", "folder"), Route::None);
    }

    #[test]
    fn cache_round_trip_and_version_check() {
        let mut routes = Routes::default();
        routes.per_source.insert("png".into(), parse_targets(PNG_OUTPUT));
        let v = cache_to_json("2.2.0", DEFAULT_LIST, &routes, true);
        let text = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        let (list, parsed, complete) = cache_from_json("2.2.0", &back).unwrap();
        assert!(complete);
        assert_eq!(list, DEFAULT_LIST);
        assert_eq!(parsed.per_source["png"], routes.per_source["png"]);
        assert!(cache_from_json("2.3.0", &back).is_none());
    }
}
