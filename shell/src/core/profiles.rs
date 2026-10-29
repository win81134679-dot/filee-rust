//! ProfileSelector, BuildFace and PresetAvailability (spec 01 §5): which profile the dropped files get, what its
//! two donut faces look like, and which slices are dimmed and why.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::formats::{self, Format, FormatTable, Route, Routes};
use crate::core::store;
use crate::model::{Faces, Slice, SliceAction};
use crate::ui::i18n;

/// Shapefile parts that belong to the `.shp` of the same name when it is in the same batch.
const COMPANIONS: [&str; 17] = [
    "shx", "dbf", "prj", "cpg", "sbn", "sbx", "fbn", "fbx", "ain", "aih", "atx", "ixs", "mxs", "qix", "qmd", "qpj", "shp.xml",
];

/// `SameAsSource`.
const SAME: &str = "*";

/// Writable single-frame raster formats the interactive crop works on.
const CROP_FORMATS: [&str; 11] = ["jpg", "png", "webp", "bmp", "avif", "jxl", "jp2", "tga", "qoi", "dds", "pcx"];

/// Formats written without an alpha channel.
const NO_ALPHA: [&str; 8] = ["jpg", "ppm", "heic", "xbm", "wbmp", "pfm", "pcx", "ras"];

/// Why a slice is dimmed: the i18n key and its argument, or nothing.
pub type Reason = Option<(&'static str, Option<String>)>;

/// Everything the face building needs, so it can be tested without the store, the CLI or the language files.
pub struct Ctx<'a> {
    pub presets: &'a [Value],
    pub profiles: &'a [Value],
    pub table: &'a FormatTable,
    pub routes: &'a Routes,
    pub t: &'a dyn Fn(&str) -> String,
    pub fmt: &'a dyn Fn(&str, &[&str]) -> String,
}

pub fn faces_for(files: &[PathBuf]) -> Faces {
    let presets = store::presets();
    let profiles = store::profiles();
    let (presets, profiles) = (as_slice(&presets), as_slice(&profiles));
    formats::with(|table, routes| {
        // Ask the builder for the source formats of this drop that it has not reached yet.
        for p in files {
            if let Some(f) = table.detect(p) {
                formats::request(&f.id);
            }
        }
        let t = |k: &str| i18n::t(k);
        let fmt = |k: &str, a: &[&str]| i18n::format(k, a);
        let ctx = Ctx { presets, profiles, table, routes, t: &t, fmt: &fmt };
        let faces = build_faces(&ctx, files);
        if faces.front.is_empty() && faces.back.is_empty() {
            // An empty donut on another computer is otherwise undiagnosable: say what it was built from.
            let first = files.first().map(|p| p.display().to_string()).unwrap_or_default();
            let detected = files.first().and_then(|p| table.detect(p)).map(|f| f.id.clone());
            crate::info!(
                "donut: no slices for {} file(s), first {first} (format {detected:?}, profile {:?}); {} presets, {} profiles, {} source formats, data {}",
                files.len(),
                faces.profile_id,
                presets.len(),
                profiles.len(),
                routes.per_source.len(),
                crate::paths::data_dir().display()
            );
        }
        faces
    })
}

fn as_slice(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

// ---------------------------------------------------------------------------------------------------------------
// Small readers over preset / profile JSON

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn b(v: &Value, path: &[&str]) -> bool {
    let mut cur = v;
    for k in path {
        match cur.get(*k) {
            Some(n) => cur = n,
            None => return false,
        }
    }
    cur.as_bool().unwrap_or(false)
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn nonblank(v: &str) -> bool {
    !v.trim().is_empty()
}

/// Unknown kinds read as Group (upstream's lenient converter); missing means Convert.
fn kind(preset: &Value) -> &'static str {
    match preset.get("kind").and_then(Value::as_str) {
        None => "Convert",
        Some(k) if k.eq_ignore_ascii_case("convert") => "Convert",
        Some(k) if k.eq_ignore_ascii_case("tool") => "Tool",
        Some(_) => "Group",
    }
}

fn is_group(p: &Value) -> bool {
    kind(p) == "Group"
}

fn is_tool(p: &Value) -> bool {
    kind(p) == "Tool"
}

fn tool_name(p: &Value) -> Option<&str> {
    p.get("tool").and_then(Value::as_str)
}

fn target(p: &Value) -> &str {
    s(p, "targetFormat")
}

fn find_preset<'a>(ctx: &Ctx<'a>, id: &str) -> Option<&'a Value> {
    ctx.presets.iter().find(|p| s(p, "id").eq_ignore_ascii_case(id))
}

// ---------------------------------------------------------------------------------------------------------------
// ProfileSelector

/// `CompanionFiles.WithoutCompanions`: a shapefile's parts are part of the `.shp` of the same name in the batch.
pub fn without_companions(table: &FormatTable, files: &[PathBuf]) -> Vec<PathBuf> {
    let stems: Vec<String> = files
        .iter()
        .filter(|f| table.extension_of(f) == "shp")
        .map(|f| stem_key(f, "shp"))
        .collect();
    files
        .iter()
        .filter(|f| {
            // ".shp.xml" is not one of the table's two-part extensions (its last part is plain xml).
            let name = f.to_string_lossy().to_lowercase();
            let ext = if name.ends_with(".shp.xml") { "shp.xml".to_string() } else { table.extension_of(f) };
            let companion = COMPANIONS.contains(&ext.as_str());
            !(companion && stems.contains(&stem_key(f, &ext)))
        })
        .cloned()
        .collect()
}

fn stem_key(path: &Path, ext: &str) -> String {
    let full = path.to_string_lossy().to_lowercase();
    full.strip_suffix(&format!(".{ext}")).unwrap_or(&full).to_string()
}

/// A profile extension list matches (`Matches`: case-insensitive, a leading dot allowed).
fn profile_has(profile: &Value, ext: &str) -> bool {
    profile
        .get("extensions")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().filter_map(Value::as_str).any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext)))
}

fn is_fallback(p: &Value) -> bool {
    b(p, &["isFallback"])
}

fn ext_count(p: &Value) -> usize {
    p.get("extensions").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
}

/// `ProfileSelector.Select`.
pub fn select_profile<'a>(ctx: &Ctx<'a>, counted: &[PathBuf]) -> Option<&'a Value> {
    let mut exts: Vec<String> = Vec::new();
    for f in counted {
        let e = ctx.table.extension_of(f);
        if !exts.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
            exts.push(e);
        }
    }
    if !exts.is_empty() {
        let all = |p: &Value| exts.iter().all(|e| profile_has(p, e));
        if let Some(p) = ctx.profiles.iter().find(|p| !is_fallback(p) && all(p)) {
            return Some(p);
        }
        if let Some(p) = ctx.profiles.iter().find(|p| is_fallback(p) && ext_count(p) > 0 && all(p)) {
            return Some(p);
        }
    }
    ctx.profiles
        .iter()
        .find(|p| is_fallback(p) && ext_count(p) == 0)
        .or_else(|| ctx.profiles.iter().find(|p| is_fallback(p)))
        .or_else(|| ctx.profiles.first())
}

// ---------------------------------------------------------------------------------------------------------------
// Names

fn display_name(ctx: &Ctx, p: &Value) -> String {
    if nonblank(s(p, "name")) {
        return s(p, "name").to_string();
    }
    match p.get("nameKey").and_then(Value::as_str) {
        Some(key) => (ctx.t)(key),
        None => target(p).to_uppercase(),
    }
}

/// `DonutName` (key + ".donut") or `RingName` (key + ".short"), else the display name.
fn suffixed_name(ctx: &Ctx, p: &Value, suffix: &str) -> String {
    if nonblank(s(p, "name")) {
        return s(p, "name").to_string();
    }
    let Some(key) = p.get("nameKey").and_then(Value::as_str) else { return display_name(ctx, p) };
    let full = format!("{key}{suffix}");
    let text = (ctx.t)(&full);
    if text == full {
        display_name(ctx, p)
    } else {
        text
    }
}

fn profile_name(ctx: &Ctx, p: &Value) -> String {
    if nonblank(s(p, "name")) {
        return s(p, "name").to_string();
    }
    match p.get("nameKey").and_then(Value::as_str) {
        Some(key) => (ctx.t)(key),
        None => s(p, "id").to_string(),
    }
}

/// `FormatLabels.NameOf`.
fn format_name(ctx: &Ctx, id: &str) -> Option<String> {
    if id == "folder" {
        return Some((ctx.t)("format.folder"));
    }
    ctx.table.by_id(id).map(|f| f.display.clone())
}

// ---------------------------------------------------------------------------------------------------------------
// PresetAvailability

fn cat_is(f: &Format, categories: &[&str]) -> bool {
    categories.contains(&f.category.as_str())
}

fn route_of(ctx: &Ctx, from: &str, to: &str) -> Route {
    ctx.routes.route(ctx.table, from, to)
}

fn usable(r: Route) -> bool {
    matches!(r, Route::Now | Route::Unknown)
}

/// The engines' names are not in the CLI's output; the target format stands in for "the engine you need".
fn needs_package(name: &str) -> Reason {
    Some(("engine.reason.needs_package", Some(name.to_string())))
}

fn reason(key: &'static str) -> Reason {
    Some((key, None))
}

/// Tools (`IsTool`) and everything `Evaluate` decides before looking at conversions.
/// Returns Some(result) when decided here.
fn tool_reason(ctx: &Ctx, preset: &Value, formats: &[&Format]) -> Option<Reason> {
    if !is_tool(preset) {
        return None;
    }
    let id = s(preset, "id");
    match tool_name(preset) {
        Some("clip") => {
            let videos: Vec<&&Format> = formats.iter().filter(|f| f.category == "Video").collect();
            if videos.is_empty() {
                return Some(reason("error.unsupported_source"));
            }
            // A clip is cut without re-encoding the format it is in (one conversion step): the video format must be
            // writable, or reachable as MP4.
            let ok = |r: Route| usable(r);
            if videos.iter().any(|f| f.writable || ok(route_of(ctx, &f.id, "mp4"))) {
                return Some(None);
            }
            if videos.iter().any(|f| route_of(ctx, &f.id, "mp4") == Route::Later) {
                return Some(needs_package("MP4"));
            }
            return Some(reason("error.no_route"));
        }
        Some("image-edit") => {
            return Some(if formats.iter().any(|f| f.category == "Image") { None } else { reason("error.images_only") });
        }
        Some("hide") => {
            return Some(if formats.iter().any(|f| f.category == "Image") { None } else { reason("error.images_only") });
        }
        _ => {}
    }
    if id == "tool-sheet-studio" {
        if !formats.is_empty() && formats.iter().all(|f| f.id == "xlsx") {
            return Some(match route_of(ctx, "xlsx", "pdf") {
                Route::Now | Route::Unknown => None,
                Route::Later => needs_package("LibreOffice"),
                Route::None => reason("error.no_route"),
            });
        }
        return Some(reason("error.unsupported_source"));
    }
    if id.starts_with("tool-pdf-") {
        return Some(if !formats.is_empty() && formats.iter().all(|f| f.id == "pdf") {
            None
        } else {
            reason("error.unsupported_source")
        });
    }
    let crop = tool_name(preset).is_none() || tool_name(preset) == Some("crop");
    if crop && !formats.is_empty() && formats.iter().all(|f| CROP_FORMATS.contains(&f.id.as_str())) {
        return Some(None);
    }
    Some(reason("error.unsupported_source"))
}

/// `PresetAvailability.Evaluate`: None when at least one of `formats` can be converted with the preset.
pub fn availability(ctx: &Ctx, preset: &Value, formats: &[&Format]) -> Reason {
    if let Some(decided) = tool_reason(ctx, preset, formats) {
        return decided;
    }
    let tgt = target(preset);

    // "Compress into one archive": any file, even of an unknown type.
    let packs = b(preset, &["archive", "combineIntoOne"])
        && ctx.table.by_id(tgt).is_some_and(|f| f.category == "Archive")
        && tgt != "folder";
    if packs {
        return if ctx.table.by_id(tgt).is_some_and(|f| f.writable) { None } else { reason("error.no_route") };
    }

    if formats.is_empty() {
        return reason("error.unsupported_source");
    }

    let mut formats: Vec<&Format> = formats.to_vec();
    let edits_picture = edits_picture(preset);
    if tgt == SAME && edits_picture {
        formats.retain(|f| f.category == "Image");
        if formats.is_empty() {
            return reason("error.images_only");
        }
    }

    if b(preset, &["image", "trimTransparent"])
        && formats.iter().all(|f| NO_ALPHA.contains(&(if tgt == SAME { f.id.as_str() } else { tgt })))
    {
        return reason("error.no_transparency");
    }

    // Merge images.
    let joins_images = b(preset, &["image", "joinIntoOne"])
        && (tgt == SAME || ctx.table.by_id(tgt).is_some_and(|f| f.category == "Image"));
    if joins_images {
        if tgt != SAME && !ctx.table.by_id(tgt).is_some_and(|f| f.writable) {
            return reason("error.no_route");
        }
        let pictures: Vec<&&Format> = formats.iter().filter(|f| cat_is(f, &["Image", "Vector"])).collect();
        if pictures.is_empty() {
            return reason("error.images_only");
        }
        // ImageMagick reads pictures as they are; vector formats need a route into PNG first.
        let readable = |r: Route| usable(r);
        if pictures.iter().any(|f| f.category == "Image" || readable(route_of(ctx, &f.id, "png"))) {
            return None;
        }
        if pictures.iter().any(|f| route_of(ctx, &f.id, "png") == Route::Later) {
            return needs_package(&display_target(ctx, "png"));
        }
        return reason("error.no_route");
    }

    // Merge videos / audio / video from images.
    let joins_media = (b(preset, &["media", "joinIntoOne"]) || b(preset, &["media", "slideshow"])) && matches!(tgt, "mp4" | "m4a");
    if joins_media {
        let slideshow = b(preset, &["media", "slideshow"]);
        let joinable = |pick: &dyn Fn(Route) -> bool| {
            formats.iter().any(|f| {
                if slideshow {
                    cat_is(f, &["Image", "Vector"])
                        && (f.id == "png" || pick(route_of(ctx, &f.id, "png")))
                        && pick(route_of(ctx, "png", tgt))
                } else {
                    (f.category == "Video" || (f.category == "Audio" && tgt == "m4a")) && pick(route_of(ctx, &f.id, tgt))
                }
            })
        };
        if joinable(&|r| usable(r)) {
            return None;
        }
        if joinable(&|r| r != Route::None) {
            return needs_package(&display_target(ctx, tgt));
        }
        return reason("error.no_route");
    }

    // An ordinary conversion: any one of the files is enough.
    let merge = b(preset, &["pdf", "mergeIntoSingle"]) && tgt == "pdf";
    let turns = turns_or_mirrors(preset);
    let ocr_targets = ["pdf", "docx", "txt", "md", "hwpx"];
    let mut copies_unturned = false;
    let mut later = false;
    for f in &formats {
        let to = if tgt == SAME { f.id.as_str() } else { tgt };
        let ocr = b(preset, &["pdf", "ocr"]) && ocr_targets.contains(&tgt) && (f.id == "pdf" || f.category == "Image");
        if merge && f.id == "pdf" && !turns && !ocr {
            return None;
        }
        let r = route_of(ctx, &f.id, to);
        if !turns {
            match r {
                Route::Now | Route::Unknown => return None,
                Route::Later => later = true,
                Route::None => {}
            }
        } else {
            // Rotate / flip: the route must pass an engine that turns pictures; those read pictures (and vector
            // drawings) and write pictures. Anything else would only be copied unturned.
            let turns_here = cat_is(f, &["Image", "Vector"])
                && ctx.table.by_id(to).is_some_and(|t| t.category == "Image");
            match r {
                Route::Now | Route::Unknown if turns_here => return None,
                Route::Now | Route::Unknown => copies_unturned = true,
                Route::Later if turns_here => later = true,
                _ => {}
            }
        }
    }
    if copies_unturned {
        return reason("error.cannot_turn");
    }
    if later {
        return needs_package(&display_target(ctx, tgt));
    }
    reason("error.no_route")
}

fn display_target(ctx: &Ctx, id: &str) -> String {
    format_name(ctx, id).unwrap_or_else(|| id.to_uppercase())
}

fn turns_or_mirrors(preset: &Value) -> bool {
    let not_none = |k: &str| preset.get("image").and_then(|i| i.get(k)).and_then(Value::as_str).is_some_and(|v| !v.eq_ignore_ascii_case("none"));
    not_none("rotation") || not_none("flip")
}

/// `ImageOptions.EditsPicture`.
fn edits_picture(preset: &Value) -> bool {
    let image = preset.get("image").unwrap_or(&Value::Null);
    let resize = image.get("resize").and_then(Value::as_str).is_some_and(|v| !v.eq_ignore_ascii_case("none"));
    let speed = image.get("animationSpeed").and_then(Value::as_f64).is_some_and(|v| v.is_finite() && v > 0.0 && v != 1.0);
    resize
        || b(image, &["grayscale"])
        || b(image, &["trimTransparent"])
        || b(image, &["removeBackground"])
        || b(image, &["maxCompression"])
        || speed
        || image.get("customSize").is_some_and(|v| !v.is_null())
        || image.get("minShortEdge").is_some_and(|v| !v.is_null())
}

fn reason_text(ctx: &Ctx, r: &Reason) -> Option<String> {
    r.as_ref().map(|(key, arg)| match arg {
        Some(a) => (ctx.fmt)(key, &[a.as_str()]),
        None => (ctx.t)(key),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// BuildFace

/// How a slice runs when picked.
fn action_of(preset: &Value, file_count: usize) -> SliceAction {
    if is_group(preset) {
        return SliceAction::Group;
    }
    if is_tool(preset) || b(preset, &["output", "askEveryTime"]) {
        return SliceAction::Hosted;
    }
    let tgt = target(preset);
    let merges_pdf = b(preset, &["pdf", "mergeIntoSingle"]) && tgt == "pdf";
    let media = (b(preset, &["media", "joinIntoOne"]) || b(preset, &["media", "slideshow"])) && matches!(tgt, "mp4" | "m4a");
    let merges_images = b(preset, &["image", "joinIntoOne"]);
    if merges_pdf || media || (merges_images && file_count > 1) {
        return SliceAction::Hosted;
    }
    SliceAction::Cli
}

pub fn build_face(ctx: &Ctx, ids: &[String], formats: &[&Format], file_count: usize) -> Vec<Slice> {
    let presets: Vec<&Value> = ids.iter().filter_map(|id| find_preset(ctx, id)).collect();
    let item = |p: &Value, caption: bool| -> Slice {
        let r = availability(ctx, p, formats);
        let tgt = target(p);
        let sub_label = if !caption {
            String::new()
        } else if tgt == SAME {
            formats.iter().take(2).map(|f| f.display.clone()).collect::<Vec<_>>().join("/")
        } else {
            format_name(ctx, tgt).unwrap_or_default()
        };
        let label = if caption { suffixed_name(ctx, p, ".donut") } else { suffixed_name(ctx, p, ".short") };
        Slice {
            preset_id: s(p, "id").to_string(),
            sub_label: if sub_label.eq_ignore_ascii_case(&label) { String::new() } else { sub_label },
            label,
            enabled: r.is_none(),
            reason: reason_text(ctx, &r),
            is_group: false,
            children: Vec::new(),
            action: action_of(p, file_count),
        }
    };
    presets
        .into_iter()
        .take(12)
        .map(|p| {
            if !is_group(p) {
                return item(p, true);
            }
            let children: Vec<Slice> = str_list(p, "subPresetIds")
                .iter()
                .filter_map(|id| find_preset(ctx, id))
                .filter(|c| !is_group(c))
                .take(6)
                .map(|c| item(c, false))
                .collect();
            let enabled = children.iter().any(|c| c.enabled);
            let reason = if enabled {
                None
            } else if children.is_empty() {
                Some((ctx.t)("donut.group_empty"))
            } else {
                children.iter().find_map(|c| c.reason.clone())
            };
            Slice {
                preset_id: s(p, "id").to_string(),
                label: display_name(ctx, p),
                sub_label: (ctx.fmt)("donut.group_caption", &[&children.len().to_string()]),
                enabled,
                reason,
                is_group: true,
                children,
                action: SliceAction::Group,
            }
        })
        .collect()
}

/// Both faces of the donut for these files.
pub fn build_faces(ctx: &Ctx, files: &[PathBuf]) -> Faces {
    let counted = without_companions(ctx.table, files);
    let Some(profile) = select_profile(ctx, &counted) else { return Faces::default() };
    // DetectFormats: known formats of the counted files, distinct, in order.
    let mut detected: Vec<&Format> = Vec::new();
    for f in &counted {
        if let Some(fmt) = ctx.table.detect(f) {
            if !detected.iter().any(|d| d.id == fmt.id) {
                detected.push(fmt);
            }
        }
    }
    let front = str_list(profile, "presetIds");
    let back = str_list(profile, "backPresetIds");
    Faces {
        profile_id: s(profile, "id").to_string(),
        profile_label: profile_name(ctx, profile),
        front: build_face(ctx, &front, &detected, counted.len()),
        back: build_face(ctx, &back, &detected, counted.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::formats::{parse_targets, Targets};

    fn j(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn id_t(k: &str) -> String {
        k.to_string()
    }

    fn id_fmt(k: &str, a: &[&str]) -> String {
        format!("{k}|{}", a.join("|"))
    }

    fn routes() -> Routes {
        let mut r = Routes::default();
        r.per_source.insert(
            "png".into(),
            parse_targets("PNG converts to:\n  jpg, webp, pdf, docx, mp4, gif\nWith optional engines (x):\n  heic, epub\n"),
        );
        r.per_source.insert("pdf".into(), parse_targets("PDF converts to:\n  png, jpg, docx, txt\n"));
        r.per_source.insert("mp4".into(), parse_targets("MP4 converts to:\n  webm, mp3, m4a, gif\n"));
        r.per_source.insert("mp3".into(), parse_targets("MP3 converts to:\n  m4a, wav\n"));
        r.per_source.insert(
            "jpg".into(),
            parse_targets("JPG converts to:
  png, webp, pdf
With optional engines (x):
  heic
"),
        );
        r.per_source.insert("dcm".into(), Targets::default());
        r.per_source.insert("xlsx".into(), parse_targets("XLSX converts to:\n  csv\nWith optional engines (x):\n  pdf\n"));
        r
    }

    struct Fixture {
        presets: Vec<Value>,
        profiles: Vec<Value>,
        routes: Routes,
    }

    impl Fixture {
        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                presets: &self.presets,
                profiles: &self.profiles,
                table: FormatTable::embedded(),
                routes: &self.routes,
                t: &id_t,
                fmt: &id_fmt,
            }
        }
    }

    fn fixture() -> Fixture {
        let presets = vec![
            j(r#"{"id":"to-png","name":"PNG","targetFormat":"png","kind":"Convert","image":{},"pdf":{},"output":{}}"#),
            j(r#"{"id":"to-jpg","name":"JPG","targetFormat":"jpg","kind":"Convert"}"#),
            j(r#"{"id":"to-heic","name":"HEIC","targetFormat":"heic","kind":"Convert"}"#),
            j(r#"{"id":"to-pdf","name":"PDF","targetFormat":"pdf","kind":"Convert"}"#),
            j(r#"{"id":"merge-pdf","nameKey":"preset.merge_pdf","targetFormat":"pdf","kind":"Convert","pdf":{"mergeIntoSingle":true}}"#),
            j(r#"{"id":"half-size","nameKey":"preset.half_size","targetFormat":"*","kind":"Convert","image":{"resize":"Percent"}}"#),
            j(r#"{"id":"trim","nameKey":"preset.trim_transparent","targetFormat":"*","kind":"Convert","image":{"trimTransparent":true}}"#),
            j(r#"{"id":"rotate-left","nameKey":"preset.rotate_left","targetFormat":"*","kind":"Convert","image":{"rotation":"Left"}}"#),
            j(r#"{"id":"rotate-right","nameKey":"preset.rotate_right","targetFormat":"*","kind":"Convert","image":{"rotation":"Right"}}"#),
            j(r#"{"id":"rotate","nameKey":"preset.group_rotate","targetFormat":"group","kind":"Group","subPresetIds":["rotate-left","rotate-right","missing","rotate"]}"#),
            j(r#"{"id":"empty-group","name":"Empty","targetFormat":"group","kind":"Group","subPresetIds":["missing"]}"#),
            j(r#"{"id":"tool-crop","nameKey":"preset.crop","targetFormat":"*","kind":"Tool"}"#),
            j(r#"{"id":"tool-clip","nameKey":"preset.clip","targetFormat":"*","kind":"Tool","tool":"clip"}"#),
            j(r#"{"id":"tool-image-edit","nameKey":"preset.image_edit","targetFormat":"*","kind":"Tool","tool":"image-edit"}"#),
            j(r#"{"id":"tool-hide","nameKey":"preset.hide","targetFormat":"*","kind":"Tool","tool":"hide"}"#),
            j(r#"{"id":"tool-sheet-studio","nameKey":"preset.sheet_studio","targetFormat":"pdf","kind":"Tool"}"#),
            j(r#"{"id":"tool-pdf-crop","nameKey":"preset.pdf_crop","targetFormat":"*","kind":"Tool"}"#),
            j(r#"{"id":"merge-images","nameKey":"preset.merge_images","targetFormat":"*","kind":"Convert","image":{"joinIntoOne":true}}"#),
            j(r#"{"id":"merge-videos","nameKey":"preset.merge_videos","targetFormat":"mp4","kind":"Convert","media":{"joinIntoOne":true}}"#),
            j(r#"{"id":"merge-audio","nameKey":"preset.merge_audio","targetFormat":"m4a","kind":"Convert","media":{"joinIntoOne":true}}"#),
            j(r#"{"id":"video-from-images","nameKey":"preset.video_from_images","targetFormat":"mp4","kind":"Convert","media":{"slideshow":true}}"#),
            j(r#"{"id":"zip-all","nameKey":"preset.zip_all","targetFormat":"zip","kind":"Convert","archive":{"combineIntoOne":true}}"#),
            j(r#"{"id":"ask","name":"Ask","targetFormat":"png","kind":"Convert","output":{"askEveryTime":true}}"#),
            j(r#"{"id":"extract","nameKey":"preset.extract","targetFormat":"folder","kind":"Convert"}"#),
        ];
        let profiles = vec![
            j(r#"{"id":"images","name":"","nameKey":"profile.images","extensions":["png","jpg","jpeg"],"isFallback":false,
                 "presetIds":["to-jpg","to-heic","half-size","rotate","tool-crop","empty-group","nope"],"backPresetIds":["to-pdf","merge-images"]}"#),
            j(r#"{"id":"pdf","name":"My PDFs","extensions":["pdf"],"presetIds":["to-png","merge-pdf"]}"#),
            j(r#"{"id":"mixed","nameKey":"profile.mixed","extensions":["png","jpg","pdf","mp4"],"isFallback":true,"presetIds":["to-png"]}"#),
            j(r#"{"id":"any","nameKey":"profile.any","extensions":[],"isFallback":true,"presetIds":["zip-all"]}"#),
        ];
        Fixture { presets, profiles, routes: routes() }
    }

    fn fmt_of<'a>(id: &str) -> &'a Format {
        FormatTable::embedded().by_id(id).unwrap()
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from(format!(r"C:\in\{n}"))).collect()
    }

    #[test]
    fn profile_selection_follows_upstream() {
        let fx = fixture();
        let ctx = fx.ctx();
        let id = |files: &[&str]| s(select_profile(&ctx, &paths(files)).unwrap(), "id").to_string();
        assert_eq!(id(&["a.png", "b.JPG"]), "images");
        assert_eq!(id(&["a.pdf"]), "pdf");
        // Mixed selection: the first fallback that holds every extension.
        assert_eq!(id(&["a.png", "b.pdf"]), "mixed");
        assert_eq!(id(&["a.png", "b.mp4"]), "mixed");
        // Something no profile holds: the catch-all (the fallback without extensions).
        assert_eq!(id(&["a.png", "b.docx"]), "any");
        assert_eq!(id(&[]), "any");
        assert!(select_profile(&Ctx { profiles: &[], ..fx.ctx() }, &paths(&["a.png"])).is_none());
    }

    #[test]
    fn catch_all_falls_back_to_first_fallback_then_first_profile() {
        let mut fx = fixture();
        fx.profiles = vec![
            j(r#"{"id":"a","extensions":["pdf"]}"#),
            j(r#"{"id":"b","extensions":["png"],"isFallback":true}"#),
        ];
        assert_eq!(s(select_profile(&fx.ctx(), &paths(&["x.docx"])).unwrap(), "id"), "b");
        fx.profiles = vec![j(r#"{"id":"a","extensions":["pdf"]}"#), j(r#"{"id":"c","extensions":["gif"]}"#)];
        assert_eq!(s(select_profile(&fx.ctx(), &paths(&["x.docx"])).unwrap(), "id"), "a");
    }

    #[test]
    fn shapefile_companions_are_part_of_the_shp() {
        let table = FormatTable::embedded();
        let files = paths(&["map.shp", "map.shx", "map.dbf", "map.prj", "other.dbf", "map.shp.xml"]);
        let counted = without_companions(table, &files);
        let names: Vec<String> = counted.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["map.shp", "other.dbf"]);
        // Without the .shp they stay.
        assert_eq!(without_companions(table, &paths(&["map.dbf"])).len(), 1);
    }

    #[test]
    fn availability_plain_conversions() {
        let fx = fixture();
        let ctx = fx.ctx();
        let to = |preset: &str, ids: &[&str]| {
            let formats: Vec<&Format> = ids.iter().map(|i| fmt_of(i)).collect();
            availability(&ctx, find_preset(&ctx, preset).unwrap(), &formats)
        };
        assert_eq!(to("to-jpg", &["png"]), None);
        // Needs an optional engine.
        assert_eq!(to("to-heic", &["png"]), Some(("engine.reason.needs_package", Some("HEIC".into()))));
        // No route at all.
        assert_eq!(to("to-heic", &["dcm"]), Some(("error.no_route", None)));
        // One usable file in a mixed selection is enough.
        assert_eq!(to("to-pdf", &["dcm", "png"]), None);
        // No known format at all.
        assert_eq!(to("to-jpg", &[]), Some(("error.unsupported_source", None)));
        // A source whose table is not built yet is optimistic.
        assert_eq!(to("to-jpg", &["bmp"]), None);
    }

    #[test]
    fn availability_same_as_source_edits() {
        let fx = fixture();
        let ctx = fx.ctx();
        let to = |preset: &str, ids: &[&str]| {
            let formats: Vec<&Format> = ids.iter().map(|i| fmt_of(i)).collect();
            availability(&ctx, find_preset(&ctx, preset).unwrap(), &formats)
        };
        assert_eq!(to("half-size", &["png"]), None);
        assert_eq!(to("half-size", &["mp3"]), Some(("error.images_only", None)));
        assert_eq!(to("half-size", &["mp3", "jpg"]), None);
        // Trim transparent: JPG has no alpha, PNG has.
        assert_eq!(to("trim", &["jpg"]), Some(("error.no_transparency", None)));
        assert_eq!(to("trim", &["jpg", "png"]), None);
        // Read-only image formats cannot be written back as they are.
        assert_eq!(to("half-size", &["dcm"]), Some(("error.no_route", None)));
    }

    #[test]
    fn availability_rotate_and_flip() {
        let fx = fixture();
        let ctx = fx.ctx();
        let to = |preset: &str, ids: &[&str]| {
            let formats: Vec<&Format> = ids.iter().map(|i| fmt_of(i)).collect();
            availability(&ctx, find_preset(&ctx, preset).unwrap(), &formats)
        };
        assert_eq!(to("rotate-left", &["png"]), None);
        // A PDF or a video would be copied unturned.
        assert_eq!(to("rotate-left", &["pdf"]), Some(("error.cannot_turn", None)));
        assert_eq!(to("rotate-left", &["mp4"]), Some(("error.cannot_turn", None)));
        assert_eq!(to("rotate-left", &["pdf", "png"]), None);
    }

    #[test]
    fn availability_tools() {
        let fx = fixture();
        let ctx = fx.ctx();
        let to = |preset: &str, ids: &[&str]| {
            let formats: Vec<&Format> = ids.iter().map(|i| fmt_of(i)).collect();
            availability(&ctx, find_preset(&ctx, preset).unwrap(), &formats)
        };
        assert_eq!(to("tool-crop", &["png", "jpg"]), None);
        assert_eq!(to("tool-crop", &["png", "gif"]), Some(("error.unsupported_source", None)));
        assert_eq!(to("tool-crop", &[]), Some(("error.unsupported_source", None)));
        assert_eq!(to("tool-clip", &["mp4"]), None);
        assert_eq!(to("tool-clip", &["png"]), Some(("error.unsupported_source", None)));
        assert_eq!(to("tool-image-edit", &["gif"]), None);
        assert_eq!(to("tool-image-edit", &["mp4"]), Some(("error.images_only", None)));
        assert_eq!(to("tool-hide", &["mp3"]), Some(("error.images_only", None)));
        assert_eq!(to("tool-pdf-crop", &["pdf"]), None);
        assert_eq!(to("tool-pdf-crop", &["pdf", "png"]), Some(("error.unsupported_source", None)));
        // Sheet studio: xlsx only, and LibreOffice (an optional engine here).
        assert_eq!(to("tool-sheet-studio", &["xlsx"]), Some(("engine.reason.needs_package", Some("LibreOffice".into()))));
        assert_eq!(to("tool-sheet-studio", &["png"]), Some(("error.unsupported_source", None)));
    }

    #[test]
    fn availability_merges() {
        let fx = fixture();
        let ctx = fx.ctx();
        let to = |preset: &str, ids: &[&str]| {
            let formats: Vec<&Format> = ids.iter().map(|i| fmt_of(i)).collect();
            availability(&ctx, find_preset(&ctx, preset).unwrap(), &formats)
        };
        // PDFs are merged as they are.
        assert_eq!(to("merge-pdf", &["pdf"]), None);
        assert_eq!(to("merge-pdf", &["png"]), None);
        assert_eq!(to("merge-images", &["png"]), None);
        assert_eq!(to("merge-images", &["mp3"]), Some(("error.images_only", None)));
        assert_eq!(to("merge-videos", &["mp4"]), None);
        // Audio only joins into M4A, not MP4.
        assert_eq!(to("merge-videos", &["mp3"]), Some(("error.no_route", None)));
        assert_eq!(to("merge-audio", &["mp3"]), None);
        assert_eq!(to("video-from-images", &["png"]), None);
        assert_eq!(to("video-from-images", &["mp3"]), Some(("error.no_route", None)));
        // "Compress into one archive" takes anything, even unknown types.
        assert_eq!(to("zip-all", &[]), None);
        assert_eq!(to("zip-all", &["mp3"]), None);
    }

    #[test]
    fn faces_are_built_with_labels_captions_and_actions() {
        let fx = fixture();
        let ctx = fx.ctx();
        let faces = build_faces(&ctx, &paths(&["a.png", "b.jpg"]));
        assert_eq!(faces.profile_id, "images");
        assert_eq!(faces.profile_label, "profile.images");
        // Missing preset ids ("nope") are dropped.
        let ids: Vec<&str> = faces.front.iter().map(|s| s.preset_id.as_str()).collect();
        assert_eq!(ids, ["to-jpg", "to-heic", "half-size", "rotate", "tool-crop", "empty-group"]);
        let jpg = &faces.front[0];
        assert_eq!((jpg.label.as_str(), jpg.sub_label.as_str()), ("JPG", "")); // caption equals the label: hidden
        assert!(jpg.enabled && jpg.reason.is_none() && jpg.action == SliceAction::Cli);
        let heic = &faces.front[1];
        assert!(!heic.enabled);
        assert_eq!(heic.reason.as_deref(), Some("engine.reason.needs_package|HEIC"));
        // "Same as source" shows the source formats.
        let half = &faces.front[2];
        assert_eq!(half.sub_label, "PNG/JPG");
        assert_eq!(half.label, "preset.half_size"); // no ".donut" text in the test language: falls back to the name
        // A group: ring of at most the existing non-group children, names without captions.
        let group = &faces.front[3];
        assert!(group.is_group && group.action == SliceAction::Group && group.enabled);
        assert_eq!(group.sub_label, "donut.group_caption|2");
        assert_eq!(group.children.len(), 2);
        assert!(group.children.iter().all(|c| c.sub_label.is_empty() && !c.is_group));
        // A tool opens a window of upstream.
        assert_eq!(faces.front[4].action, SliceAction::Hosted);
        // A group with nothing left says so.
        let empty = &faces.front[5];
        assert!(!empty.enabled && empty.children.is_empty());
        assert_eq!(empty.reason.as_deref(), Some("donut.group_empty"));
        // Back face.
        let back: Vec<&str> = faces.back.iter().map(|s| s.preset_id.as_str()).collect();
        assert_eq!(back, ["to-pdf", "merge-images"]);
        // Merge images of several files needs the order question: hosted; of one file it does not.
        assert_eq!(faces.back[1].action, SliceAction::Hosted);
        let single = build_faces(&ctx, &paths(&["a.png"]));
        assert_eq!(single.back[1].action, SliceAction::Cli);
    }

    #[test]
    fn group_reason_is_the_first_child_reason_when_all_are_dimmed() {
        let fx = fixture();
        let ctx = fx.ctx();
        let faces = build_faces(&ctx, &paths(&["a.png", "b.jpg"]));
        assert!(faces.front[3].enabled);
        // For a PDF both rotations copy unturned.
        let mut fx2 = fixture();
        fx2.profiles = vec![j(r#"{"id":"p","extensions":["pdf"],"presetIds":["rotate"]}"#)];
        let faces = build_faces(&fx2.ctx(), &paths(&["a.pdf"]));
        let g = &faces.front[0];
        assert!(!g.enabled);
        assert_eq!(g.reason.as_deref(), Some("error.cannot_turn"));
    }

    #[test]
    fn actions_by_preset() {
        let fx = fixture();
        let ctx = fx.ctx();
        let action = |id: &str, n: usize| action_of(find_preset(&ctx, id).unwrap(), n);
        assert_eq!(action("to-png", 3), SliceAction::Cli);
        assert_eq!(action("merge-pdf", 2), SliceAction::Hosted);
        assert_eq!(action("merge-videos", 2), SliceAction::Hosted);
        assert_eq!(action("merge-audio", 1), SliceAction::Hosted);
        assert_eq!(action("video-from-images", 5), SliceAction::Hosted);
        assert_eq!(action("ask", 1), SliceAction::Hosted);
        assert_eq!(action("rotate", 1), SliceAction::Group);
        assert_eq!(action("tool-clip", 1), SliceAction::Hosted);
        assert_eq!(action("zip-all", 4), SliceAction::Cli);
        assert_eq!(action("extract", 1), SliceAction::Cli);
    }

    #[test]
    fn kind_reads_leniently() {
        assert_eq!(kind(&j(r#"{}"#)), "Convert");
        assert_eq!(kind(&j(r#"{"kind":"Tool"}"#)), "Tool");
        assert_eq!(kind(&j(r#"{"kind":"Whatever"}"#)), "Group");
    }
}
