//! Tests that run upstream's real `filee-cli.exe` and write data files. They are `#[ignore]`d and refuse to run unless
//! `FILEE_DATA_DIR` points under `shell\target\core-test` (a copy of the upstream data, never `%APPDATA%\Filee`):
//!
//! ```text
//! $env:FILEE_DATA_DIR = "<project>\shell\target\core-test\data"   # copies of settings/presets/profiles/history.json
//! cargo test -- --ignored --test-threads=1 core::integration_tests
//! ```
//! Output goes to `shell\target\core-test\out\`; the data copy is put back after each test.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::core::jobs::{self, FileState, JobSpec};
use crate::core::store::test_support::{integration_lock, isolated_data_dir, job_events, project_dir};
use crate::core::store::{self, DataFile};
use crate::core::{formats, history, integration, util, watch};
use crate::model::JobState;

fn out_dir(name: &str) -> PathBuf {
    let dir = project_dir().join("shell").join("target").join("core-test").join("out").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_file(name: &str) -> PathBuf {
    let p = project_dir().join("build").join("test").join(name);
    assert!(p.exists(), "missing test file {}", p.display());
    p
}

/// Keeps a data file's bytes and puts them back when dropped.
struct Restore(PathBuf, Option<Vec<u8>>);

impl Restore {
    fn of(file: DataFile) -> Restore {
        let path = isolated_data_dir().join(file.file_name());
        let bytes = std::fs::read(&path).ok();
        Restore(path, bytes)
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        match &self.1 {
            Some(b) => {
                let _ = std::fs::write(&self.0, b);
            }
            None => {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _ = store::reload_if_changed();
    }
}

fn load_store() {
    isolated_data_dir();
    store::reload_if_changed();
    assert!(store::find_preset("to-png").is_some(), "the data copy has no to-png preset");
}

#[test]
#[ignore]
fn cli_conversion_reports_progress_and_results() {
    let _lock = integration_lock();
    load_store();
    let out = out_dir("flow1");
    let pdf = test_file("hosted-test.pdf");
    let started = Instant::now();
    let outcome = jobs::run_blocking(&JobSpec {
        preset_id: "to-png".into(),
        files: vec![pdf.clone()],
        output_folder: Some(out.clone()),
        ..Default::default()
    });
    println!("to-png took {:?}", started.elapsed());
    let events = job_events();
    println!("events: {:?}", events.iter().map(|e| (e.state, e.done, e.total)).collect::<Vec<_>>());
    assert!(events.len() >= 3, "start, a result, the end");
    assert_eq!(events.first().unwrap().state, JobState::Running);
    assert_eq!(events.first().unwrap().done, 0);
    assert!(events.iter().any(|e| e.state == JobState::Running && e.done == 1));
    let last = events.last().unwrap();
    assert_eq!(last.state, JobState::Completed);
    assert_eq!((last.total, last.done, last.failed), (1, 1, 0));
    assert_eq!(last.title.is_empty(), false);
    assert_eq!(outcome.status.outputs.len(), 1);
    assert!(out.join("hosted-test.png").exists(), "{:?}", outcome.status.outputs);
    let detail = jobs::take_detail(outcome.id).expect("detail kept for history");
    assert_eq!(detail.results.len(), 1);
    assert_eq!(detail.results[0].state, FileState::Done);
    assert_eq!(util::path_key(&detail.results[0].source), util::path_key(&pdf));
}

#[test]
#[ignore]
fn cli_failures_skips_and_missing_files() {
    let _lock = integration_lock();
    load_store();
    let out = out_dir("flow2");
    let bad = out.join("bad.png");
    std::fs::write(&bad, "not a png").unwrap();
    let good = test_file("hosted-test.png");
    let o = jobs::run_blocking(&JobSpec {
        preset_id: "to-jpg".into(),
        files: vec![bad.clone(), good.clone()],
        output_folder: Some(out.clone()),
        ..Default::default()
    });
    assert_eq!(o.status.state, JobState::CompletedWithErrors, "{:?}", o.status);
    assert_eq!((o.status.done, o.status.failed), (1, 1));
    let failed = o.results.iter().find(|r| r.state == FileState::Failed).unwrap();
    assert_eq!(util::path_key(&failed.source), util::path_key(&bad));
    assert!(failed.error.as_deref().unwrap_or("").contains("error"), "{:?}", failed.error);
    assert!(out.join("hosted-test.jpg").exists());
    // Nothing worked: Failed.
    let o = jobs::run_blocking(&JobSpec {
        preset_id: "to-jpg".into(),
        files: vec![bad.clone()],
        output_folder: Some(out.clone()),
        ..Default::default()
    });
    assert_eq!(o.status.state, JobState::Failed);
    // A path that does not exist: the CLI exits 2 with a message on stderr.
    let o = jobs::run_blocking(&JobSpec {
        preset_id: "to-jpg".into(),
        files: vec![out.join("nonexistent.png")],
        ..Default::default()
    });
    assert_eq!(o.status.state, JobState::Failed);
    assert!(o.status.errors.iter().any(|e| e.contains("not found")), "{:?}", o.status.errors);
    // A group is not a conversion.
    let o = jobs::run_blocking(&JobSpec { preset_id: "rotate".into(), files: vec![good], ..Default::default() });
    assert_eq!(o.status.state, JobState::Failed);
    assert!(o.status.errors.iter().any(|e| e.contains("group")), "{:?}", o.status.errors);
    job_events();
}

#[test]
#[ignore]
fn cli_merge_uses_json_and_reports_the_merged_file() {
    let _lock = integration_lock();
    load_store();
    let out = out_dir("flow3");
    let o = jobs::run_blocking(&JobSpec {
        preset_id: "merge-pdf".into(),
        files: vec![test_file("hosted-test.png"), test_file("hosted-test.pdf")],
        output_folder: Some(out.clone()),
        ..Default::default()
    });
    assert_eq!(o.status.state, JobState::Completed, "{:?}", o.status);
    assert_eq!(o.status.outputs.len(), 1, "{:?}", o.status.outputs);
    assert!(o.status.outputs[0].exists());
    assert!(o.status.outputs[0].to_string_lossy().ends_with("_merged.pdf"));
    assert_eq!(o.results.len(), 2);
    job_events();
}

#[test]
#[ignore]
fn cli_job_can_be_cancelled() {
    let _lock = integration_lock();
    load_store();
    let out = out_dir("flow4");
    // Many copies of a PDF → PNG at 300 DPI: long enough to cancel in the middle.
    let src = out_dir("flow4-in");
    let files: Vec<PathBuf> = (0..40)
        .map(|i| {
            let p = src.join(format!("copy-{i:02}.pdf"));
            std::fs::copy(test_file("hosted-test.pdf"), &p).unwrap();
            p
        })
        .collect();
    let id = jobs::run(JobSpec {
        preset_id: "to-png".into(),
        files,
        output_folder: Some(out.clone()),
        ..Default::default()
    });
    std::thread::sleep(Duration::from_millis(900));
    let note = jobs::cancel(id);
    println!("cancel note: {note:?}");
    assert!(note.is_some(), "the job was still running");
    let deadline = Instant::now() + Duration::from_secs(20);
    let last = loop {
        let events = job_events();
        if let Some(e) = events.iter().rev().find(|e| e.id == id && e.state != JobState::Running) {
            break e.clone();
        }
        assert!(Instant::now() < deadline, "no final event");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(last.state, JobState::Cancelled);
    let detail = jobs::take_detail(id).unwrap();
    assert_eq!(detail.results.len(), 40, "every source has a result, cancelled ones included");
    assert!(detail.results.iter().any(|r| r.state == FileState::Cancelled));
    println!("{} converted before cancelling", detail.results.iter().filter(|r| r.state == FileState::Done).count());
    assert!(jobs::cancel(id).is_none(), "gone after it ended");
}

#[test]
#[ignore]
fn history_entry_is_written_in_front_and_read_back() {
    let _lock = integration_lock();
    load_store();
    let _restore = Restore::of(DataFile::History);
    let before: Value = util::read_json_file(&DataFile::History.path()).unwrap_or(json!([]));
    let before_len = before.as_array().unwrap().len();
    let out = out_dir("flow5");
    let o = jobs::run_blocking(&JobSpec {
        preset_id: "to-png".into(),
        files: vec![test_file("hosted-test.pdf")],
        output_folder: Some(out.clone()),
        ..Default::default()
    });
    history::record(&o.status);
    let after = util::read_json_file(&DataFile::History.path()).unwrap();
    let list = after.as_array().unwrap();
    assert_eq!(list.len(), before_len + 1);
    let e = &list[0];
    assert_eq!(e["presetId"], "to-png");
    assert_eq!(e["state"], "Completed");
    assert_eq!(e["sources"][0].as_str().unwrap().to_lowercase(), test_file("hosted-test.pdf").to_string_lossy().to_lowercase());
    assert_eq!(e["outputs"].as_array().unwrap().len(), 1);
    assert_eq!(e["undoable"][0]["length"], std::fs::metadata(out.join("hosted-test.png")).unwrap().len());
    assert!(e["jobId"].as_str().unwrap().len() == 10);
    println!("finishedAt {}  writtenUtc {}", e["finishedAt"], e["undoable"][0]["writtenUtc"]);
    // Non-ASCII (the project folder has Chinese in it) is escaped like upstream writes it.
    assert!(std::fs::read_to_string(DataFile::History.path()).unwrap().is_ascii());
}

#[test]
#[ignore]
fn store_writes_only_what_it_changes() {
    let _lock = integration_lock();
    load_store();
    let _s = Restore::of(DataFile::Settings);
    let _p = Restore::of(DataFile::Presets);
    let original = std::fs::read_to_string(DataFile::Settings.path()).unwrap();
    let was = store::settings().get("paused").and_then(Value::as_bool).unwrap_or(false);
    store::set_paused(!was);
    let changed = std::fs::read_to_string(DataFile::Settings.path()).unwrap();
    assert_ne!(original, changed);
    let line_diff: Vec<(&str, &str)> = original.lines().zip(changed.lines()).filter(|(a, b)| a != b).collect();
    println!("changed lines: {line_diff:?}");
    assert_eq!(line_diff.len(), 1, "only the paused line differs");
    assert_eq!(original.lines().count(), changed.lines().count());
    assert_eq!(original.contains("\r\n"), changed.contains("\r\n"), "line endings kept");
    assert_eq!(store::settings()["paused"], json!(!was));
    // Our own write is not reported back as an external change.
    assert!(!store::reload_if_changed());
    store::set_paused(was);
    assert_eq!(std::fs::read_to_string(DataFile::Settings.path()).unwrap(), original);

    // 300 DPI on the presets copy: changes only dpi fields, second pass changes nothing.
    let presets_before = std::fs::read_to_string(DataFile::Presets.path()).unwrap();
    let before_value: Value = serde_json::from_str(&presets_before).unwrap();
    let planned = integration::plan_300_dpi(&before_value).len();
    store::mutate(DataFile::Presets, "300dpi", Arc::new(integration::apply_300_dpi));
    let presets_after = std::fs::read_to_string(DataFile::Presets.path()).unwrap();
    println!("300 DPI: {planned} planned change(s); file changed: {}", presets_before != presets_after);
    assert_eq!(presets_before != presets_after, planned > 0);
    let after_value: Value = serde_json::from_str(&presets_after).unwrap();
    assert!(integration::plan_300_dpi(&after_value).is_empty());
    assert_eq!(before_value.as_array().unwrap().len(), after_value.as_array().unwrap().len());
    let t0 = std::fs::metadata(DataFile::Presets.path()).unwrap().modified().unwrap();
    store::mutate(DataFile::Presets, "300dpi", Arc::new(integration::apply_300_dpi));
    assert_eq!(std::fs::metadata(DataFile::Presets.path()).unwrap().modified().unwrap(), t0, "no write when nothing changes");
}

#[test]
#[ignore]
fn real_formats_output_matches_the_parsers() {
    let cli = crate::paths::filee_cli();
    let run = |args: &[&str]| {
        let out = util::hidden_command(&cli).args(args).output().unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let version = run(&["version"]);
    println!("version: {}", version.trim());
    let list = run(&["formats"]);
    let live = formats::FormatTable::parse(&list);
    let embedded = formats::FormatTable::embedded();
    assert_eq!(live.formats.len(), embedded.formats.len(), "the embedded list is out of date for this upstream");
    let started = Instant::now();
    let png = formats::parse_targets(&run(&["formats", "png"]));
    println!("formats png: {} now, {} later ({:?})", png.now.len(), png.later.len(), started.elapsed());
    assert!(png.now.contains("jpg") && png.now.contains("pdf"));
    let dcm = formats::parse_targets(&run(&["formats", "dcm"]));
    println!("formats dcm: {} now, {} later", dcm.now.len(), dcm.later.len());
    // A read-only source and an extension spelled as the CLI accepts it.
    let raw = formats::parse_targets(&run(&["formats", "raw"]));
    assert!(!raw.now.is_empty());
}

#[test]
#[ignore]
fn background_build_fills_the_table_and_writes_the_cache() {
    let _lock = integration_lock();
    let cache = crate::paths::lite_dir().join("formats-2.2.0.json");
    let _ = std::fs::remove_file(&cache);
    let started = Instant::now();
    formats::start_background();
    // A drag of a PNG asks for its source format first.
    formats::request("png");
    let deadline = Instant::now() + Duration::from_secs(400);
    loop {
        let (n, done) = formats::progress();
        if done {
            println!("built {n} source formats in {:?}", started.elapsed());
            break;
        }
        assert!(Instant::now() < deadline, "not finished after {n} formats");
        std::thread::sleep(Duration::from_secs(2));
    }
    let table_png = formats::with(|t, r| r.route(t, "png", "jpg"));
    assert_eq!(table_png, formats::Route::Now);
    let cached: Value = util::read_json_file(&cache).expect("cache file");
    assert_eq!(cached["complete"], true);
    assert_eq!(cached["version"], "2.2.0");
    let n = cached["targets"].as_object().unwrap().len();
    assert!(n > 250, "{n}");
    println!("cache: {} bytes", std::fs::metadata(&cache).unwrap().len());
    let _ = std::fs::remove_file(&cache);
}

fn watch_rule(folder: &Path, preset: &str) -> watch::Rule {
    watch::Rule { id: "feedc0de".into(), folder: folder.to_path_buf(), preset_id: preset.into(), ..Default::default() }
}

/// Runs the watcher by hand: scan, wait out the settle time virtually, convert what is ready.
fn pump(w: &mut watch::Watcher, at: Instant) -> usize {
    w.scan(at);
    let ready = w.take_ready(at + Duration::from_secs(3));
    let n = ready.len();
    if n > 0 {
        watch::run_batch(w, ready);
    }
    n
}

#[test]
#[ignore]
fn watch_folder_converts_new_files_and_moves_originals() {
    let _lock = integration_lock();
    load_store();
    let dir = out_dir("watch1");
    let mut rule = watch_rule(&dir, "to-jpg");
    rule.include_subfolders = true;
    rule.move_originals = true;
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    // One file was there before the watcher started, one arrives later in a subfolder.
    std::fs::copy(test_file("hosted-test.png"), dir.join("before.png")).unwrap();
    let mut w = watch::Watcher::new(rule);
    let t0 = Instant::now();
    assert_eq!(pump(&mut w, t0), 1, "originals moved: the old file counts");
    assert!(dir.join("converted").join("before.jpg").exists());
    assert!(dir.join("originals").join("before.png").exists(), "source moved");
    assert!(!dir.join("before.png").exists());
    std::fs::copy(test_file("hosted-test.png"), dir.join("sub").join("later.png")).unwrap();
    assert_eq!(pump(&mut w, t0 + Duration::from_secs(5)), 1);
    assert!(dir.join("converted").join("sub").join("later.jpg").exists(), "subfolder structure carried into the output");
    assert!(dir.join("originals").join("sub").join("later.png").exists());
    // Nothing more to do; the outputs are not converted again.
    assert_eq!(pump(&mut w, t0 + Duration::from_secs(20)), 0);
    let events = job_events();
    assert_eq!(events.iter().filter(|e| e.state == JobState::Completed).count(), 2);
    let detail_ids: Vec<u64> = events.iter().filter(|e| e.state == JobState::Completed).map(|e| e.id).collect();
    for id in detail_ids {
        let d = jobs::take_detail(id).unwrap();
        assert_eq!(d.watch_rule_id.as_deref(), Some("feedc0de"));
    }
}

#[test]
#[ignore]
fn watch_folder_with_name_rules_uses_a_temporary_preset() {
    let _lock = integration_lock();
    load_store();
    let dir = out_dir("watch2");
    let mut rule = watch_rule(&dir, "to-jpg");
    rule.output_folder = dir.join("out").to_string_lossy().into_owned();
    rule.file_name_pattern = "{name}_x".into();
    rule.renames = vec![json!({"kind": "Uppercase", "find": "", "replace": ""})];
    let mut w = watch::Watcher::new(rule);
    let t0 = Instant::now();
    assert_eq!(pump(&mut w, t0), 0);
    std::fs::copy(test_file("hosted-test.png"), dir.join("top.png")).unwrap();
    assert_eq!(pump(&mut w, t0 + Duration::from_secs(1)), 1);
    let produced: Vec<String> =
        std::fs::read_dir(dir.join("out")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    println!("produced: {produced:?}");
    assert_eq!(produced, ["TOP_X.jpg"]);
    let events = job_events();
    let last = events.last().unwrap();
    assert_eq!(last.preset_id, "to-jpg", "reported with the real preset id");
    assert_eq!(last.state, JobState::Completed);
    // The temporary data folder is gone.
    let leftovers = std::fs::read_dir(std::env::temp_dir().join("FileeLite"))
        .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("watch-feedc0de")).count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
}

/// A computer where upstream Filee never ran has no presets.json / profiles.json (upstream keeps its built-in library
/// in memory until something is saved). The donut must still show upstream's default slices, not an empty ring.
#[test]
#[ignore]
fn first_run_without_library_files_still_fills_the_donut() {
    let _lock = integration_lock();
    isolated_data_dir();
    let (_p, _q) = (Restore::of(DataFile::Presets), Restore::of(DataFile::Profiles));
    let _ = std::fs::remove_file(isolated_data_dir().join(DataFile::Presets.file_name()));
    let _ = std::fs::remove_file(isolated_data_dir().join(DataFile::Profiles.file_name()));
    store::load();
    let faces = crate::core::profiles::faces_for(&[test_file("hosted-test.png")]);
    assert!(!faces.front.is_empty(), "the donut is empty when the library files are missing");
    assert!(store::find_preset("to-png").is_some());
    assert!(isolated_data_dir().join(DataFile::Profiles.file_name()).exists(), "upstream's defaults were not written");
}

/// The PDF donut: merge-pdf gives way to "轉成 Office" (DOCX, XLSX, PPTX), and each member really converts.
#[test]
#[ignore]
fn pdf_donut_has_the_office_group_and_it_converts() {
    let _lock = integration_lock();
    let (_p, _q) = (Restore::of(DataFile::Presets), Restore::of(DataFile::Profiles));
    let marker = isolated_data_dir().join("filee-rust-policy.json");
    let _ = std::fs::remove_file(&marker);
    load_store();
    integration::apply_pdf_office_once();
    store::reload_if_changed();
    assert!(marker.exists());
    let pdf = test_file("hosted-test.pdf");
    let faces = crate::core::profiles::faces_for(std::slice::from_ref(&pdf));
    let all: Vec<_> = faces.front.iter().chain(&faces.back).collect();
    assert!(all.iter().all(|s| s.preset_id != "merge-pdf"), "merge-pdf is still on the PDF donut");
    let group = all.iter().find(|s| s.preset_id == integration::PDF_OFFICE_GROUP).expect("no Office group");
    println!("group {:?} on the {} face", group.label, if faces.front.iter().any(|s| s.preset_id == group.preset_id) { "front" } else { "back" });
    let kids: Vec<_> = group.children.iter().map(|c| (c.preset_id.as_str(), c.enabled)).collect();
    println!("children {kids:?}");
    assert_eq!(kids.iter().map(|k| k.0).collect::<Vec<_>>(), ["to-docx", "to-xlsx", "to-pptx"]);
    let out = out_dir("pdf-office");
    // PDF → XLSX takes tables drawn with lines (upstream's rule), so that one gets a PDF with a ruled table.
    let table = test_file("table-test.pdf");
    for (id, ext, input) in [("to-docx", "docx", &pdf), ("to-xlsx", "xlsx", &table), ("to-pptx", "pptx", &pdf)] {
        let outcome = jobs::run_blocking(&JobSpec {
            preset_id: id.into(),
            files: vec![input.clone()],
            output_folder: Some(out.clone()),
            ..Default::default()
        });
        let made = std::fs::read_dir(&out).unwrap().filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == ext)).map(|e| e.metadata().unwrap().len()).max();
        println!("{id}: {:?} -> {made:?} bytes", outcome.status.state);
        assert!(made.is_some_and(|n| n > 0), "{id} produced no .{ext}");
    }
    let _ = std::fs::remove_file(&marker);
}

/// The self-update as filee-rust.exe runs it, against a fake GitHub (FILEE_RUST_UPDATE_API, inherited by the child):
/// FILEE_LITE_APP = a Filee.exe build with --self-update, FILEE_RUST_TEST_KEY = the test public key,
/// FILEE_RUST_TEST_DIR = an empty download folder. The fake serves 2.2.0-rust.6 for every machine.
#[test]
#[ignore]
fn self_update_finds_downloads_and_verifies_a_newer_release() {
    use crate::core::self_update::{self, Line};
    use std::io::{BufRead, BufReader};
    let (Some(key), Some(dir)) = (std::env::var_os("FILEE_RUST_TEST_KEY"), std::env::var_os("FILEE_RUST_TEST_DIR")) else {
        panic!("set FILEE_RUST_TEST_KEY, FILEE_RUST_TEST_DIR and FILEE_RUST_UPDATE_API");
    };
    let run = |current: &str| -> Vec<Line> {
        let mut child = self_update::command(current, std::path::Path::new(&key), "AAAA-BBBB", std::path::Path::new(&dir))
            .spawn()
            .unwrap();
        let lines: Vec<Line> = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
            .map(|l| self_update::parse_line(&l))
            .filter(|l| !matches!(l, Line::Progress(..)))
            .collect();
        assert!(child.wait().unwrap().success());
        lines
    };
    let newer = run("2.2.0-rust.5");
    println!("{newer:?}");
    match newer.last() {
        Some(Line::Ready(v, path)) => {
            assert_eq!(v, "2.2.0-rust.6");
            assert!(path.exists(), "{}", path.display());
        }
        other => panic!("expected ready, got {other:?}"),
    }
    assert_eq!(run("2.2.0-rust.6"), [Line::Current("2.2.0-rust.6".into())]);
}
