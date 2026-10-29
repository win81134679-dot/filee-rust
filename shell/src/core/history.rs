//! history.json entries for jobs the CLI ran (spec 02 §2): the CLI writes none, and upstream's "Recent conversions"
//! page, Undo and Retry read this file. Entries go in at the front, the list is cut at `historyLimit`
//! (clamped 50..2000), and while upstream's hosted Filee is open the write waits (see `store::mutate`).

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::core::jobs::{self, FileResult, FileState};
use crate::core::store::{self, DataFile};
use crate::core::util;
use crate::model::{JobState, JobStatus};

/// More results than this get no Undo (upstream `MaxUndoable`).
const MAX_UNDOABLE: usize = 500;

/// Adds a finished job to history.json (not for a job that is still running).
pub fn record(job: &JobStatus) {
    if job.state == JobState::Running {
        return;
    }
    let settings = store::settings();
    if settings.get("keepHistory").and_then(Value::as_bool) == Some(false) {
        return;
    }
    let limit = settings.get("historyLimit").and_then(Value::as_i64).unwrap_or(200).clamp(50, 2000) as usize;
    let detail = jobs::take_detail(job.id);
    let preset = store::find_preset(&job.preset_id);
    let preset_name = jobs::display_name_of(&job.preset_id);
    let info = |p: &Path| file_info(p);
    let exists = |p: &Path| p.exists();
    let entry = build_entry(
        job,
        detail.as_ref(),
        preset.as_ref(),
        &preset_name,
        &util::new_job_id(job.id),
        &util::local_iso_now(),
        &info,
        &exists,
    );
    store::mutate(
        DataFile::History,
        "history",
        Arc::new(move |v| {
            if !v.is_array() {
                *v = Value::Array(Vec::new());
            }
            let list = v.as_array_mut().expect("array");
            list.insert(0, entry.clone());
            list.truncate(limit);
            true
        }),
    );
}

/// Length and last-write time (FILETIME) of a file, or None when it is not a file.
fn file_info(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some((meta.len(), util::filetime_of(meta.modified().ok()?)))
}

fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Running => "Running",
        JobState::Completed => "Completed",
        JobState::CompletedWithErrors => "CompletedWithErrors",
        JobState::Failed => "Failed",
        JobState::Cancelled => "Cancelled",
    }
}

fn paths_json<'a>(paths: impl Iterator<Item = &'a Path>) -> Value {
    Value::Array(paths.map(|p| Value::String(p.to_string_lossy().into_owned())).collect())
}

/// One history entry, properties in upstream's declaration order.
#[allow(clippy::too_many_arguments)]
pub fn build_entry(
    job: &JobStatus,
    detail: Option<&jobs::Detail>,
    preset: Option<&Value>,
    preset_name: &str,
    job_id: &str,
    finished_at: &str,
    file_info: &dyn Fn(&Path) -> Option<(u64, u64)>,
    path_exists: &dyn Fn(&Path) -> bool,
) -> Value {
    let results: &[FileResult] = detail.map(|d| d.results.as_slice()).unwrap_or(&[]);
    let sources: Vec<&Path> = match detail {
        Some(d) => d.sources.iter().map(|p| p.as_path()).collect(),
        None => Vec::new(),
    };
    let outputs: Vec<&Path> = if results.is_empty() {
        job.outputs.iter().map(|p| p.as_path()).collect()
    } else {
        results.iter().flat_map(|r| r.outputs.iter().map(|p| p.as_path())).collect()
    };
    let failures: Vec<Value> = results
        .iter()
        .filter(|r| r.state == FileState::Failed)
        .map(|r| {
            let mut f = Map::new();
            f.insert("source".into(), Value::String(r.source.to_string_lossy().into_owned()));
            if let Some(e) = r.error.as_ref().filter(|e| !e.is_empty()) {
                f.insert("errorDetail".into(), Value::String(e.clone()));
            }
            Value::Object(f)
        })
        .collect();

    let mut entry = Map::new();
    entry.insert("jobId".into(), json!(job_id));
    entry.insert("finishedAt".into(), json!(finished_at));
    entry.insert("presetId".into(), json!(job.preset_id));
    entry.insert("presetName".into(), json!(preset_name));
    entry.insert("state".into(), json!(state_name(job.state)));
    entry.insert("sources".into(), paths_json(sources.iter().copied()));
    entry.insert("outputs".into(), paths_json(outputs.iter().copied()));
    if let Some(rule) = detail.and_then(|d| d.watch_rule_id.as_ref()) {
        entry.insert("watchRuleId".into(), json!(rule));
    }
    let has_failures = !failures.is_empty();
    entry.insert("failures".into(), Value::Array(failures));
    if has_failures {
        // A copy for Retry: the PDF password stays out of history.json.
        if let Some(p) = preset {
            if !p.get("image").and_then(|i| i.get("hiddenAreas")).and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
                entry.insert("preset".into(), without_password(p));
            }
        }
    }
    entry.insert("errors".into(), Value::Array(Vec::new()));
    entry.insert("undoable".into(), Value::Array(undoable(results, file_info, path_exists)));
    entry.insert("originalsInTrash".into(), Value::Bool(false));
    entry.insert("undone".into(), Value::Bool(false));
    Value::Object(entry)
}

/// `Preset.WithoutPassword(mark: true)`.
pub fn without_password(preset: &Value) -> Value {
    let mut copy = preset.clone();
    if let Some(pdf) = copy.get_mut("pdf").and_then(Value::as_object_mut) {
        if pdf.get("password").and_then(Value::as_str).is_some_and(|p| !p.is_empty()) {
            pdf.insert("password".into(), Value::String(String::new()));
            pdf.insert("passwordLeftOut".into(), Value::Bool(true));
        }
    }
    copy
}

/// `HistoryEntry.UndoableOutputs`: what Undo may move later, with the size and write time it checks.
pub fn undoable(
    results: &[FileResult],
    file_info: &dyn Fn(&Path) -> Option<(u64, u64)>,
    path_exists: &dyn Fn(&Path) -> bool,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for r in results {
        for output in &r.outputs {
            // A source left as it was ("only if smaller" with "replace original") is the user's file, not a result.
            if util::path_key(output) == util::path_key(&r.source) {
                continue;
            }
            // An output folder, or a document whose pictures sit in "<name>_files" beside it: no Undo for the job.
            let stem = output.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let files_dir = output.parent().map(|p| p.join(format!("{stem}_files")));
            let Some((len, written)) = file_info(output) else { return Vec::new() };
            if files_dir.is_some_and(|d| path_exists(&d)) {
                return Vec::new();
            }
            let key = util::path_key(output);
            if seen.contains(&key) {
                continue;
            }
            if out.len() == MAX_UNDOABLE {
                return Vec::new();
            }
            seen.push(key);
            out.push(json!({
                "path": output.to_string_lossy(),
                "length": len,
                "writtenUtc": util::utc_iso(written),
                "trashed": false,
            }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn result(src: &str, state: FileState, outs: &[&str], err: Option<&str>) -> FileResult {
        FileResult { source: p(src), state, outputs: outs.iter().map(|o| p(o)).collect(), error: err.map(String::from) }
    }

    fn status(state: JobState) -> JobStatus {
        JobStatus {
            id: 7,
            title: "PNG".into(),
            preset_id: "to-png".into(),
            total: 2,
            done: 1,
            failed: 1,
            state,
            outputs: vec![],
            errors: vec![],
        }
    }

    fn info_ok(path: &Path) -> Option<(u64, u64)> {
        // Mirrors upstream's sample: 73014 bytes, 2026-10-28T17:48:28.9016082Z.
        path.extension().map(|_| (73_014, 116_444_736_000_000_000u64 + 1_793_209_708u64 * 10_000_000 + 9_016_082))
    }

    fn no_dir(_: &Path) -> bool {
        false
    }

    #[test]
    fn entry_matches_upstream_layout() {
        let detail = jobs::Detail {
            sources: vec![p(r"C:\in\a.pdf"), p(r"C:\in\bad.pdf")],
            results: vec![
                result(r"C:\in\a.pdf", FileState::Done, &[r"C:\in\a.png"], None),
                result(r"C:\in\bad.pdf", FileState::Failed, &[], Some("The engine reported an error (x)")),
            ],
            watch_rule_id: Some("1a2b3c4d".into()),
        };
        let preset: Value = serde_json::from_str(r#"{"id":"to-png","name":"PNG","pdf":{"password":"secret"}}"#).unwrap();
        let entry = build_entry(
            &status(JobState::CompletedWithErrors),
            Some(&detail),
            Some(&preset),
            "PNG",
            "432364e516",
            "2026-10-29T01:48:28.9071793+08:00",
            &info_ok,
            &no_dir,
        );
        let keys: Vec<&String> = entry.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["jobId", "finishedAt", "presetId", "presetName", "state", "sources", "outputs", "watchRuleId", "failures", "preset", "errors", "undoable", "originalsInTrash", "undone"]
        );
        assert_eq!(entry["state"], "CompletedWithErrors");
        assert_eq!(entry["watchRuleId"], "1a2b3c4d");
        assert_eq!(entry["outputs"], json!([r"C:\in\a.png"]));
        assert_eq!(entry["failures"], json!([{"source": r"C:\in\bad.pdf", "errorDetail": "The engine reported an error (x)"}]));
        // The Retry copy has no password and says so.
        assert_eq!(entry["preset"]["pdf"]["password"], "");
        assert_eq!(entry["preset"]["pdf"]["passwordLeftOut"], true);
        assert_eq!(entry["undoable"][0]["length"], 73_014);
        assert_eq!(entry["undoable"][0]["writtenUtc"], "2026-10-28T17:48:28.9016082Z");
        assert_eq!(entry["undoable"][0]["trashed"], false);
    }

    #[test]
    fn clean_job_has_no_preset_copy_or_watch_rule() {
        let detail = jobs::Detail {
            sources: vec![p(r"C:\in\a.pdf")],
            results: vec![result(r"C:\in\a.pdf", FileState::Done, &[r"C:\in\a.png"], None)],
            watch_rule_id: None,
        };
        let preset: Value = serde_json::from_str(r#"{"id":"to-png"}"#).unwrap();
        let entry = build_entry(&status(JobState::Completed), Some(&detail), Some(&preset), "PNG", "id", "t", &info_ok, &no_dir);
        let obj = entry.as_object().unwrap();
        assert!(!obj.contains_key("preset") && !obj.contains_key("watchRuleId"));
        assert_eq!(obj["failures"], json!([]));
    }

    #[test]
    fn undo_rules() {
        let outs = |names: &[&str]| vec![result(r"C:\in\a.pdf", FileState::Done, names, None)];
        // Normal.
        assert_eq!(undoable(&outs(&[r"C:\o\a.png", r"C:\o\b.png"]), &info_ok, &no_dir).len(), 2);
        // Duplicates (case-insensitive) count once.
        assert_eq!(undoable(&outs(&[r"C:\o\a.png", r"c:\O\A.PNG"]), &info_ok, &no_dir).len(), 1);
        // The source itself is not a result.
        assert_eq!(undoable(&outs(&[r"C:\in\a.pdf", r"C:\o\a.png"]), &info_ok, &no_dir).len(), 1);
        // An output that is not a file (a folder, or gone): none for the whole job.
        assert!(undoable(&outs(&[r"C:\o\a.png", r"C:\o\folder"]), &info_ok, &no_dir).is_empty());
        // A "<name>_files" folder beside an output: none.
        let files_dir = |d: &Path| d.ends_with("a_files");
        assert!(undoable(&outs(&[r"C:\o\a.html"]), &info_ok, &files_dir).is_empty());
        // More than 500: none.
        let many: Vec<String> = (0..501).map(|i| format!(r"C:\o\p{i}.png")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(undoable(&outs(&refs), &info_ok, &no_dir).is_empty());
        let fewer: Vec<&str> = refs[..500].to_vec();
        assert_eq!(undoable(&outs(&fewer), &info_ok, &no_dir).len(), 500);
    }

    #[test]
    fn running_jobs_are_not_recorded() {
        // Must not touch any file: returns before reading settings.
        record(&status(JobState::Running));
    }

    #[test]
    fn password_is_left_out_only_when_there_was_one() {
        let with: Value = serde_json::from_str(r#"{"pdf":{"password":"x","passwordLeftOut":false}}"#).unwrap();
        assert_eq!(without_password(&with)["pdf"]["passwordLeftOut"], true);
        let without: Value = serde_json::from_str(r#"{"pdf":{"password":"","passwordLeftOut":false}}"#).unwrap();
        assert_eq!(without_password(&without), without);
    }
}
