//! System integration and policy (spec 02 §5.4): the Explorer verb for files, and the "pictures are 300 DPI" policy
//! on upstream's presets (a port of `工具\filee_300dpi.py`).
//!
//! No Run key (start with Windows is the installer's job), and the registry is only written by an installed
//! filee-rust.exe: never from `shell\target\` and never with FILEE_DATA_DIR set (development and tests).

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

use crate::core::store::{self, DataFile};
use crate::core::util;
use crate::ui::i18n;
use crate::{info, warn};

pub const TARGET_DPI: i64 = 300;

/// Raster output formats; `*` is a same-format picture edit (rotate, grayscale, halve …).
const RASTER_TARGETS: [&str; 15] = [
    "png", "jpg", "jpeg", "webp", "tiff", "bmp", "gif", "avif", "heic", "jxl", "psd", "tga", "ico", "icns", "*",
];

/// Everything derived from settings.json that touches the system or upstream's data.
pub fn apply(settings: &Value) {
    if force_300_dpi() {
        store::mutate(DataFile::Presets, "300dpi", Arc::new(apply_300_dpi));
    }
    apply_pdf_groups_once();
    let enabled = settings.get("contextMenuEnabled").and_then(Value::as_bool).unwrap_or(true);
    apply_context_menu(enabled);
}

// ---------------------------------------------------------------------------------------------------------------
// PDF donut groups Filee Rust adds (user's requests 2026-10-29), each once per user:
// - "merge PDF" (only useful with several files) becomes "轉成 Office" (DOCX, XLSX, PPTX);
// - "split PDF" becomes "PDF 合併與拆分" (merge, split, page editor, add PDF, compress).

/// A group put on the PDF donut in place of one preset.
pub struct PdfGroup {
    /// Key in `policy.json` that marks it done.
    pub policy: &'static str,
    pub id: &'static str,
    pub name: &'static str,
    pub members: &'static [&'static str],
    /// The slice it takes the place of (front or back face of the PDF profile).
    pub replaces: &'static str,
}

/// Id of the Office group.
pub const PDF_OFFICE_GROUP: &str = "lite-pdf-office";
/// Id of the merge / split group.
pub const PDF_TOOLS_GROUP: &str = "lite-pdf-tools";

pub const PDF_GROUPS: [PdfGroup; 2] = [
    PdfGroup {
        policy: "pdfOfficeGroup",
        id: PDF_OFFICE_GROUP,
        name: "轉成 Office",
        members: &["to-docx", "to-xlsx", "to-pptx"],
        replaces: "merge-pdf",
    },
    PdfGroup {
        policy: "pdfToolsGroup",
        id: PDF_TOOLS_GROUP,
        name: "PDF 合併與拆分",
        members: &["merge-pdf", "pdf-split", "tool-pdf-pages", "tool-pdf-add", "compress-pdf"],
        replaces: "pdf-split",
    },
];

/// Once per user and group (`policy.json` in Filee Rust's folder): after that, what the user does in upstream's
/// editor stands.
pub fn apply_pdf_groups_once() {
    // With FILEE_DATA_DIR (development, tests) the marker stays beside that data copy, never in the real folder.
    let marker = match std::env::var_os("FILEE_DATA_DIR") {
        Some(_) => crate::paths::data_dir().join("filee-rust-policy.json"),
        None => crate::paths::lite_dir().join("policy.json"),
    };
    let mut policy = util::read_json_file(&marker).filter(Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    if store::presets().as_array().is_none_or(|a| a.is_empty()) {
        return; // no library yet (seeding failed): try again next start
    }
    let mut marked = false;
    for group in &PDF_GROUPS {
        if policy.get(group.policy).and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let group_written = store::mutate(DataFile::Presets, group.policy, Arc::new(|v: &mut Value| add_pdf_group(v, group)));
        // No group to copy the fields from (an edited library): leave the slice alone rather than point at nothing.
        let group_exists = store::presets().as_array().is_some_and(|a| a.iter().any(|p| p["id"] == group.id));
        if !group_exists {
            continue;
        }
        let swap_written = store::mutate(DataFile::Profiles, group.id, Arc::new(|v: &mut Value| swap_in_pdf_profile(v, group)));
        // Queued (the settings window is open) or refused: not done yet. The next pass finds the group there and
        // nothing left to change, and only then is it marked done.
        if group_written && swap_written {
            policy[group.policy] = Value::Bool(true);
            marked = true;
            info!("PDF donut: {} replaced by the group '{}' (once)", group.replaces, group.name);
        }
    }
    if marked {
        if let Err(e) = util::atomic_write(&marker, util::to_upstream_json(&policy, false).as_bytes()) {
            warn!("Could not write {}: {e}", marker.display());
        }
    }
}

/// Adds the group preset, copied from upstream's own `pdf-edit` group so every field upstream expects is there.
pub fn add_pdf_group(presets: &mut Value, group: &PdfGroup) -> bool {
    let Some(list) = presets.as_array_mut() else { return false };
    if list.iter().any(|p| p["id"] == group.id) {
        return false;
    }
    let Some(mut preset) = list.iter().find(|p| p["targetFormat"] == "group").cloned() else { return false };
    preset["id"] = group.id.into();
    preset["name"] = group.name.into();
    preset["nameKey"] = "".into();
    preset["subPresetIds"] = group.members.iter().map(|&s| Value::from(s)).collect();
    list.push(preset);
    true
}

/// In the PDF profile only, the slice `group.replaces` (front or back) becomes the group. Groups that merely contain
/// that preset (upstream's `pdf-edit` holds `pdf-split`) are left as they are.
pub fn swap_in_pdf_profile(profiles: &mut Value, group: &PdfGroup) -> bool {
    let Some(pdf) = profiles.as_array_mut().and_then(|a| a.iter_mut().find(|p| p["id"] == "pdf")) else { return false };
    let mut changed = false;
    for face in ["presetIds", "backPresetIds"] {
        if let Some(ids) = pdf.get_mut(face).and_then(Value::as_array_mut) {
            if ids.iter().any(|i| i == group.id) {
                return false;
            }
            for id in ids.iter_mut().filter(|i| *i == group.replaces) {
                *id = group.id.into();
                changed = true;
            }
        }
    }
    changed
}

// ---------------------------------------------------------------------------------------------------------------
// 300 DPI

/// `lite.json` in Filee Rust's folder: `{"force300Dpi": true}`; on when the file or the key is missing.
pub fn force_300_dpi() -> bool {
    util::read_json_file(&crate::paths::lite_dir().join("lite.json"))
        .and_then(|v| v.get("force300Dpi").and_then(Value::as_bool))
        .unwrap_or(true)
}

/// A field that needs to change: preset id, field name, old value.
#[cfg(test)]
pub type Change = (String, &'static str, Value);

#[cfg(test)]
pub fn plan_300_dpi(presets: &Value) -> Vec<Change> {
    let mut changes = Vec::new();
    for p in presets.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(pdf) = p.get("pdf").filter(|v| v.is_object()) {
            let old = pdf.get("renderDpi");
            if !old.and_then(Value::as_f64).is_some_and(|v| v >= TARGET_DPI as f64) {
                changes.push((id.clone(), "pdf.renderDpi", old.cloned().unwrap_or(Value::Null)));
            }
        }
        let raster = p.get("targetFormat").and_then(Value::as_str).is_some_and(|t| RASTER_TARGETS.contains(&t));
        if let Some(image) = p.get("image").filter(|v| v.is_object()) {
            let old = image.get("dpi");
            if raster && old.and_then(Value::as_f64) != Some(TARGET_DPI as f64) {
                changes.push((id, "image.dpi", old.cloned().unwrap_or(Value::Null)));
            }
        }
    }
    changes
}

/// Sets pdf.renderDpi (when below 300) and image.dpi (for picture outputs) to 300. True when something changed.
pub fn apply_300_dpi(presets: &mut Value) -> bool {
    let mut changed = false;
    let Some(list) = presets.as_array_mut() else { return false };
    for p in list {
        let raster = p.get("targetFormat").and_then(Value::as_str).is_some_and(|t| RASTER_TARGETS.contains(&t));
        if let Some(pdf) = p.get_mut("pdf").and_then(Value::as_object_mut) {
            if !pdf.get("renderDpi").and_then(Value::as_f64).is_some_and(|v| v >= TARGET_DPI as f64) {
                pdf.insert("renderDpi".into(), Value::from(TARGET_DPI));
                changed = true;
            }
        }
        if raster {
            if let Some(image) = p.get_mut("image").and_then(Value::as_object_mut) {
                if image.get("dpi").and_then(Value::as_f64) != Some(TARGET_DPI as f64) {
                    image.insert("dpi".into(), Value::from(TARGET_DPI));
                    changed = true;
                }
            }
        }
    }
    changed
}

// ---------------------------------------------------------------------------------------------------------------
// Explorer verb for files: HKCU\Software\Classes\*\shell\FileeLite

pub const VERB_KEY: &str = r"Software\Classes\*\shell\FileeLite";
const VERB_COMMAND_KEY: &str = r"Software\Classes\*\shell\FileeLite\command";

/// One registry string to set: key (under HKCU), value name ("" is the default value), data.
pub type RegValue = (String, String, String);

/// What the verb consists of for an installed `exe` and a menu title.
pub fn context_menu_values(exe: &Path, title: &str) -> Vec<RegValue> {
    let exe = exe.to_string_lossy();
    vec![
        (VERB_KEY.into(), "MUIVerb".into(), title.into()),
        (VERB_KEY.into(), "Icon".into(), format!("\"{exe}\",0")),
        (VERB_KEY.into(), "MultiSelectModel".into(), "Player".into()),
        (VERB_COMMAND_KEY.into(), String::new(), format!("\"{exe}\" --convert \"%1\"")),
    ]
}

/// True for a binary under `shell\target\` (a cargo build): it must not register itself anywhere.
pub fn is_development_build(exe: &Path) -> bool {
    util::path_key(exe).contains("\\shell\\target\\")
}

/// Registry writes are allowed for an installed exe only.
fn registry_allowed() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if is_development_build(&exe) || std::env::var_os("FILEE_DATA_DIR").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    Some(exe)
}

fn apply_context_menu(enabled: bool) {
    let Some(exe) = registry_allowed() else { return };
    if enabled {
        let title = i18n::t("general.context_menu_label");
        for (key, name, data) in context_menu_values(&exe, &title) {
            if read_string(&key, &name).as_deref() != Some(data.as_str()) {
                match write_string(&key, &name, &data) {
                    Ok(()) => info!("Explorer verb: set {key}\\{name}"),
                    Err(code) => warn!("Explorer verb: could not set {key}\\{name} (error {code})"),
                }
            }
        }
    } else if read_string(VERB_COMMAND_KEY, "").is_some() {
        match delete_tree(VERB_KEY) {
            Ok(()) => info!("Explorer verb removed"),
            Err(code) => warn!("Explorer verb: could not remove it (error {code})"),
        }
    }
}

fn read_string(subkey: &str, name: &str) -> Option<String> {
    let sub = util::wide(subkey);
    let val = util::wide(name);
    let mut size = 0u32;
    unsafe {
        let name_ptr = if name.is_empty() { PCWSTR::null() } else { PCWSTR(val.as_ptr()) };
        if RegGetValueW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), name_ptr, RRF_RT_REG_SZ, None, None, Some(&mut size)) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u16; (size as usize / 2) + 1];
        let mut size2 = (buf.len() * 2) as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            name_ptr,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size2),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

fn write_string(subkey: &str, name: &str, data: &str) -> Result<(), u32> {
    let sub = util::wide(subkey);
    let val = util::wide(name);
    let bytes: Vec<u8> = util::wide(data).iter().flat_map(|c| c.to_le_bytes()).collect();
    unsafe {
        let mut key = HKEY::default();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if rc != ERROR_SUCCESS {
            return Err(rc.0);
        }
        let name_ptr = if name.is_empty() { PCWSTR::null() } else { PCWSTR(val.as_ptr()) };
        let rc = RegSetValueExW(key, name_ptr, None, REG_SZ, Some(&bytes));
        let _ = RegCloseKey(key);
        if rc != ERROR_SUCCESS {
            return Err(rc.0);
        }
    }
    Ok(())
}

fn delete_tree(subkey: &str) -> Result<(), u32> {
    let sub = util::wide(subkey);
    let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr())) };
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(rc.0)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn presets() -> Value {
        serde_json::from_str(
            r#"[
              {"id":"to-png","targetFormat":"png","image":{"dpi":150,"quality":90},"pdf":{"renderDpi":150,"zeta":1}},
              {"id":"to-jpg","targetFormat":"jpg","image":{"quality":90},"pdf":{"renderDpi":300}},
              {"id":"to-mp3","targetFormat":"mp3","image":{"dpi":72},"pdf":{"renderDpi":72}},
              {"id":"half-size","targetFormat":"*","image":{"dpi":300}},
              {"id":"to-docx","targetFormat":"docx"},
              {"id":"odd","targetFormat":"png","image":{"dpi":300},"pdf":{"renderDpi":600}},
              {"id":"bad","targetFormat":"png","image":"x","pdf":{"renderDpi":"fast"}}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn plan_lists_what_the_python_tool_would() {
        let plan = plan_300_dpi(&presets());
        let got: Vec<(String, &str)> = plan.iter().map(|(i, f, _)| (i.clone(), *f)).collect();
        assert_eq!(
            got,
            [
                ("to-png".to_string(), "pdf.renderDpi"),
                ("to-png".to_string(), "image.dpi"),
                ("to-jpg".to_string(), "image.dpi"),
                ("to-mp3".to_string(), "pdf.renderDpi"),
                ("bad".to_string(), "pdf.renderDpi"),
            ]
        );
        assert_eq!(plan[0].2, 150);
        assert_eq!(plan[2].2, Value::Null);
    }

    #[test]
    fn apply_sets_both_fields_and_is_idempotent() {
        let mut v = presets();
        assert!(apply_300_dpi(&mut v));
        let at = |i: usize| v[i].clone();
        assert_eq!(at(0)["pdf"]["renderDpi"], 300);
        assert_eq!(at(0)["image"]["dpi"], 300);
        // Other keys and their order stay.
        assert_eq!(at(0)["pdf"].as_object().unwrap().keys().collect::<Vec<_>>(), ["renderDpi", "zeta"]);
        assert_eq!(at(0)["image"].as_object().unwrap().keys().collect::<Vec<_>>(), ["dpi", "quality"]);
        // A missing dpi is added at the end of the image options.
        assert_eq!(at(1)["image"].as_object().unwrap().keys().collect::<Vec<_>>(), ["quality", "dpi"]);
        // Not a picture output: image.dpi untouched; pdf.renderDpi still raised.
        assert_eq!(at(2)["image"]["dpi"], 72);
        assert_eq!(at(2)["pdf"]["renderDpi"], 300);
        // Higher than 300 stays.
        assert_eq!(at(5)["pdf"]["renderDpi"], 600);
        // A non-number is replaced; a non-object image is left alone.
        assert_eq!(at(6)["pdf"]["renderDpi"], 300);
        assert_eq!(at(6)["image"], "x");
        // No pdf/image blocks: nothing added.
        assert!(at(4).get("pdf").is_none() && at(4).get("image").is_none());
        assert!(!apply_300_dpi(&mut v));
        assert!(plan_300_dpi(&v).is_empty());
        assert!(!apply_300_dpi(&mut Value::Null));
    }

    #[test]
    fn context_menu_values_are_what_the_verb_needs() {
        let vals = context_menu_values(&PathBuf::from(r"C:\Program Files\Filee Rust\filee-rust.exe"), "Convert with Filee");
        assert_eq!(vals[0], (VERB_KEY.into(), "MUIVerb".into(), "Convert with Filee".into()));
        assert_eq!(vals[1].2, r#""C:\Program Files\Filee Rust\filee-rust.exe",0"#);
        let command = vals.iter().find(|(k, n, _)| k.ends_with(r"\command") && n.is_empty()).unwrap();
        assert_eq!(command.2, r#""C:\Program Files\Filee Rust\filee-rust.exe" --convert "%1""#);
        assert!(vals.iter().all(|(k, _, _)| k.starts_with(r"Software\Classes\*\shell\FileeLite")));
    }

    #[test]
    fn development_builds_never_write_the_registry() {
        assert!(is_development_build(Path::new(r"C:\Work\Filee\shell\target\debug\filee-rust.exe")));
        assert!(is_development_build(Path::new(r"C:\Work\Filee\shell\target\release\deps\filee_lite-1a2b.exe")));
        assert!(!is_development_build(Path::new(r"C:\Program Files\Filee Rust\filee-rust.exe")));
        // The running test binary is itself a development build.
        assert!(registry_allowed().is_none());
        // And applying with the menu on or off from here touches nothing.
        apply_context_menu(true);
        apply_context_menu(false);
    }

    fn library() -> (Value, Value) {
        let presets = serde_json::from_str(
            r#"[
              {"id":"to-docx","name":"DOCX","targetFormat":"docx"},
              {"id":"merge-pdf","name":"","nameKey":"preset.merge_pdf","targetFormat":"pdf"},
              {"id":"pdf-edit","name":"","nameKey":"preset.group_pdf_edit","targetFormat":"group","image":{"quality":90},
               "kind":"Group","subPresetIds":["tool-pdf-crop","pdf-split"]}
            ]"#,
        )
        .unwrap();
        let profiles = serde_json::from_str(
            r#"[
              {"id":"images","presetIds":["to-png","merge-pdf"]},
              {"id":"pdf","presetIds":["to-png","to-docx","pdf-split","merge-pdf"],"backPresetIds":["pdf-edit"]}
            ]"#,
        )
        .unwrap();
        (presets, profiles)
    }

    #[test]
    fn pdf_office_group_is_built_from_upstreams_own_group() {
        let (mut presets, _) = library();
        let office = &PDF_GROUPS[0];
        assert!(add_pdf_group(&mut presets, office));
        let group = presets.as_array().unwrap().iter().find(|p| p["id"] == PDF_OFFICE_GROUP).unwrap();
        assert_eq!(group["targetFormat"], "group");
        assert_eq!(group["kind"], "Group");
        assert_eq!(group["nameKey"], "");
        assert_eq!(group["name"], "轉成 Office");
        assert_eq!(group["subPresetIds"], serde_json::json!(["to-docx", "to-xlsx", "to-pptx"]));
        // Fields upstream needs on every preset come from the template.
        assert_eq!(group["image"]["quality"], 90);
        // Once there, nothing changes.
        assert!(!add_pdf_group(&mut presets, office));
    }

    #[test]
    fn pdf_tools_group_holds_merge_split_and_the_page_tools() {
        let (mut presets, _) = library();
        assert!(add_pdf_group(&mut presets, &PDF_GROUPS[1]));
        let group = presets.as_array().unwrap().iter().find(|p| p["id"] == PDF_TOOLS_GROUP).unwrap();
        assert_eq!(group["name"], "PDF 合併與拆分");
        assert_eq!(
            group["subPresetIds"],
            serde_json::json!(["merge-pdf", "pdf-split", "tool-pdf-pages", "tool-pdf-add", "compress-pdf"])
        );
    }

    #[test]
    fn both_groups_take_their_slices_on_the_pdf_profile_only() {
        let (_, mut profiles) = library();
        assert!(swap_in_pdf_profile(&mut profiles, &PDF_GROUPS[0]));
        assert!(swap_in_pdf_profile(&mut profiles, &PDF_GROUPS[1]));
        assert_eq!(profiles[1]["presetIds"], serde_json::json!(["to-png", "to-docx", PDF_TOOLS_GROUP, PDF_OFFICE_GROUP]));
        // Upstream's pdf-edit group on the back still holds pdf-split; it is a member there, not a slice.
        assert_eq!(profiles[1]["backPresetIds"], serde_json::json!(["pdf-edit"]));
        // Other profiles keep merge-pdf (multi-select merges of pictures).
        assert_eq!(profiles[0]["presetIds"][1], "merge-pdf");
        // Done once: nothing changes the second time.
        assert!(!swap_in_pdf_profile(&mut profiles, &PDF_GROUPS[0]));
        assert!(!swap_in_pdf_profile(&mut profiles, &PDF_GROUPS[1]));
    }

    #[test]
    fn pdf_profile_without_the_slice_is_left_alone() {
        let mut profiles = serde_json::json!([{"id":"pdf","presetIds":["to-png"]}]);
        for group in &PDF_GROUPS {
            assert!(!swap_in_pdf_profile(&mut profiles, group));
            assert!(!swap_in_pdf_profile(&mut Value::Null, group));
        }
    }
}
