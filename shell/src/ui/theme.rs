//! The [`Palette`] for settings.json's `theme` (spec 03 §4): Soft or Material You colours, accent presets,
//! `theme.json` overrides, corner radius, font chain and the reduce-motion switch.
//! `compute` is pure (an [`Env`] carries everything read from Windows); `palette` fills the Env.

use serde_json::Value;
use windows::core::{w, PCWSTR};
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::model::Palette;

const DEFAULT_ACCENT: u32 = 0xFF7F77DD;
const DEFAULT_RADIUS: f32 = 16.0;
/// `theme.json` larger than this is ignored (upstream's limit).
const THEME_JSON_MAX_BYTES: u64 = 64 * 1024;

/// (key, light accent, dark accent) of upstream's accent presets.
const PRESETS: [(&str, u32, u32); 8] = [
    ("monochrome", 0xFF1F1E1D, 0xFFF1EFE8),
    ("lavender", 0xFF7F77DD, 0xFFBCAEE5),
    ("peach", 0xFFE07A70, 0xFFF29B91),
    ("mint", 0xFF529B7D, 0xFF88D5B1),
    ("rose", 0xFFE86483, 0xFFF79BB0),
    ("sky", 0xFF5B8BE3, 0xFF98B8F6),
    ("amber", 0xFFCB852A, 0xFFF0BA6A),
    ("sage", 0xFF5A8E51, 0xFF8DC983),
];

/// What `compute` needs from the machine.
pub struct Env<'a> {
    /// Windows "app mode" is dark.
    pub system_dark: bool,
    /// Windows "Show animations" is off.
    pub system_reduce_motion: bool,
    /// Parsed `theme.json`, if any.
    pub custom: Option<Value>,
    /// Language code in use (`zh-TW` …), for the font chain.
    pub language: &'a str,
    /// Is this font family installed?
    pub font_installed: &'a dyn Fn(&str) -> bool,
}

/// The palette for the current settings, reading Windows' light/dark mode, animation setting and fonts.
pub fn palette(settings: &Value) -> Palette {
    let installed = font_checker();
    let env = Env {
        system_dark: !apps_use_light_theme(),
        system_reduce_motion: !client_area_animation(),
        custom: load_theme_json(),
        language: &super::i18n::current(),
        font_installed: &installed,
    };
    compute(settings, &env)
}

pub fn compute(settings: &Value, env: &Env) -> Palette {
    let theme = settings.get("theme").unwrap_or(&Value::Null);
    let text_of = |key: &str| theme.get(key).and_then(Value::as_str).unwrap_or("");

    let dark = match text_of("mode").to_ascii_lowercase().as_str() {
        "dark" => true,
        "light" => false,
        _ => env.system_dark,
    };
    let radius = theme.get("cornerRadius").and_then(Value::as_f64).map(|r| r as f32).unwrap_or(DEFAULT_RADIUS);
    let radius = radius.clamp(0.0, 28.0).round();

    // A preset wins over `accent`; each has a light and a dark colour.
    let seed = match PRESETS.iter().find(|(key, _, _)| *key == text_of("presetKey")) {
        Some((_, light, dark_accent)) => {
            if dark {
                *dark_accent
            } else {
                *light
            }
        }
        None => parse_hex(text_of("accent")).unwrap_or(DEFAULT_ACCENT),
    };
    let material = text_of("style").eq_ignore_ascii_case("MaterialYou");
    let mut s = if material { material_you(seed, dark) } else { soft(seed, dark) };

    if let Some(custom) = &env.custom {
        apply_overrides(&mut s, custom, dark);
    }

    let reduce_motion = theme.get("reduceMotion").and_then(Value::as_bool).unwrap_or(env.system_reduce_motion);
    Palette {
        dark,
        accent: s.accent,
        accent_text: s.on_accent,
        surface: s.surface,
        surface_alt: s.surface_alt,
        text: s.text,
        text_muted: s.text_muted,
        border: s.border,
        danger: s.danger,
        success: s.success,
        corner_radius: radius,
        fonts: installed_fonts(env.language, env.font_installed),
        reduce_motion,
    }
}

/// The colours of one variant, before they go into a [`Palette`].
#[derive(Clone, Copy, Debug)]
struct Scheme {
    surface: u32,
    surface_alt: u32,
    border: u32,
    text: u32,
    text_muted: u32,
    danger: u32,
    success: u32,
    accent: u32,
    on_accent: u32,
}

fn soft(seed: u32, dark: bool) -> Scheme {
    let (accent, on_accent) = if is_monochrome_preset(seed) {
        if dark {
            (0xFFF1EFE8, 0xFF1F1E1D)
        } else {
            (0xFF1F1E1D, 0xFFFFFFFF)
        }
    } else {
        (seed, if luminance(seed) > 0.6 { 0xFF1F1E1D } else { 0xFFFFFFFF })
    };
    if dark {
        Scheme {
            surface: 0xFF262624,
            surface_alt: 0xFF31312E,
            border: 0x1CFFFFFF,
            text: 0xFFF1EFE8,
            text_muted: 0xFFAEACA4,
            danger: 0xFFF09595,
            success: 0xFF5DCAA5,
            accent,
            on_accent,
        }
    } else {
        Scheme {
            surface: 0xFFFFFFFF,
            surface_alt: 0xFFEFEDE6,
            border: 0x16000000,
            text: 0xFF1F1E1D,
            text_muted: 0xFF6B6A66,
            danger: 0xFFC73A39,
            success: 0xFF0F7A5A,
            accent,
            on_accent,
        }
    }
}

fn is_monochrome_preset(seed: u32) -> bool {
    seed == 0xFF1F1E1D || seed == 0xFFF1EFE8
}

fn material_you(seed: u32, dark: bool) -> Scheme {
    let (h, s, l) = rgb_to_hsl(seed);
    if is_monochrome(seed) {
        return if dark {
            Scheme {
                surface: 0xFF222220,
                surface_alt: 0xFF2C2C29,
                border: 0x1CFFFFFF,
                text: 0xFFF1EFE8,
                text_muted: 0xFFAEACA4,
                danger: 0xFFF09595,
                success: 0xFF5DCAA5,
                accent: 0xFFF1EFE8,
                on_accent: 0xFF1F1E1D,
            }
        } else {
            Scheme {
                surface: 0xFFFFFFFF,
                surface_alt: 0xFFEEECE5,
                border: 0x18000000,
                text: 0xFF1F1E1D,
                text_muted: 0xFF6E6C67,
                danger: 0xFFC73A39,
                success: 0xFF0F7A5A,
                accent: 0xFF1F1E1D,
                on_accent: 0xFFFFFFFF,
            }
        };
    }
    let sat = s.clamp(0.20, 0.85);
    if dark {
        let primary = if l < 0.65 { hsl_to_rgb(h, (sat * 0.75).min(0.65), 0.75) } else { seed };
        Scheme {
            surface: hsl_to_rgb(h, (sat * 0.20).min(0.14), 0.13),
            surface_alt: hsl_to_rgb(h, (sat * 0.22).min(0.16), 0.18),
            border: with_alpha(primary, 32),
            text: hsl_to_rgb(h, 0.08, 0.92),
            text_muted: hsl_to_rgb(h, 0.12, 0.75),
            danger: 0xFFFFB4AB,
            success: 0xFF8CD692,
            accent: primary,
            on_accent: if luminance(primary) > 0.45 { 0xFF16141A } else { 0xFFFFFFFF },
        }
    } else {
        let primary = if l > 0.60 { hsl_to_rgb(h, sat, 0.45) } else { seed };
        Scheme {
            surface: 0xFFFFFFFF,
            surface_alt: hsl_to_rgb(h, (sat * 0.22).min(0.16), 0.92),
            border: with_alpha(primary, 28),
            text: hsl_to_rgb(h, 0.10, 0.12),
            text_muted: hsl_to_rgb(h, 0.15, 0.38),
            danger: 0xFFBA1A1A,
            success: 0xFF1B6D24,
            accent: primary,
            on_accent: if luminance(primary) > 0.55 { 0xFF1F1E1D } else { 0xFFFFFFFF },
        }
    }
}

/// Upstream's `IsMonochrome`: nearly grey seeds get the fixed grey Material You scheme.
fn is_monochrome(seed: u32) -> bool {
    let (_, s, _) = rgb_to_hsl(seed);
    let (r, g, b) = channels(seed);
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    s < 0.08 || (r == g && g == b) || ((r - g).abs() <= 4 && (g - b).abs() <= 4 && (r - b).abs() <= 4)
}

/// `theme.json`: `dark` / `light` blocks (case-sensitive), colour names case-insensitive.
fn apply_overrides(s: &mut Scheme, custom: &Value, dark: bool) {
    let Some(block) = custom.get(if dark { "dark" } else { "light" }).and_then(Value::as_object) else {
        return;
    };
    for (name, value) in block {
        let Some(color) = value.as_str().and_then(parse_hex) else { continue };
        match name.to_ascii_lowercase().as_str() {
            "surface" => s.surface = color,
            "surfacealt" => s.surface_alt = color,
            "border" => s.border = color,
            "text" => s.text = color,
            "textsecondary" => s.text_muted = color,
            "danger" => s.danger = color,
            "success" => s.success = color,
            _ => {} // page and warning are not drawn by Filee Lite
        }
    }
}

/// `#RRGGBB` or `#AARRGGBB` to 0xAARRGGBB.
pub fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(|v| 0xFF00_0000 | v),
        8 => u32::from_str_radix(hex, 16).ok(),
        _ => None,
    }
}

pub fn channels(c: u32) -> (u8, u8, u8) {
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub fn with_alpha(c: u32, a: u8) -> u32 {
    (u32::from(a) << 24) | (c & 0x00FF_FFFF)
}

/// Upstream's `Shade`: `a >= 0` mixes towards white, `a < 0` towards black; the alpha is kept.
pub fn shade(c: u32, a: f32) -> u32 {
    let f = |v: u8| -> u32 {
        let v = f32::from(v);
        let out = if a >= 0.0 { v + (255.0 - v) * a } else { v * (1.0 + a) };
        out.round_ties_even().clamp(0.0, 255.0) as u32
    };
    let (r, g, b) = channels(c);
    (c & 0xFF00_0000) | (f(r) << 16) | (f(g) << 8) | f(b)
}

/// Upstream's non-linear luminance, 0..1.
pub fn luminance(c: u32) -> f32 {
    let (r, g, b) = channels(c);
    (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
}

fn rgb_to_hsl(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = channels(c);
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;
    if delta <= 1e-5 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { delta / (2.0 - max - min) } else { delta / (max + min) };
    let h = if max == r {
        ((g - b) / delta + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> u32 {
    let to_byte = |x: f32| (x * 255.0).round_ties_even().clamp(0.0, 255.0) as u32;
    if s < 1e-5 {
        let v = to_byte(l);
        return 0xFF00_0000 | (v << 16) | (v << 8) | v;
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    0xFF00_0000 | (to_byte(channel(h + 1.0 / 3.0)) << 16) | (to_byte(channel(h)) << 8) | to_byte(channel(h - 1.0 / 3.0))
}

// ---- fonts -----------------------------------------------------------------------------------------------

/// Upstream's UI font chain for `language` (spec §4.7), including families that may not be installed.
pub fn font_chain(language: &str) -> Vec<&'static str> {
    let lang = language.to_ascii_lowercase();
    let mut chain = vec!["Noto Sans", "Noto Sans Arabic", "Noto Sans Devanagari", "Noto Sans Thai"];
    if super::i18n::is_traditional_chinese(&lang) {
        chain.extend(["Microsoft JhengHei UI", "Microsoft JhengHei", "PingFang TC"]);
    }
    // (embedded Noto family, system families) for Japanese, Korean, Simplified Chinese.
    let jp: (&str, &[&str]) = ("Noto Sans JP", &["Yu Gothic UI", "Meiryo", "Hiragino Sans"]);
    let kr: (&str, &[&str]) = ("Noto Sans KR", &["Malgun Gothic", "Apple SD Gothic Neo"]);
    let sc: (&str, &[&str]) = ("Noto Sans SC", &["Microsoft YaHei", "PingFang SC"]);
    let order = if lang.starts_with("ja") {
        [jp, kr, sc]
    } else if lang.starts_with("zh") {
        [sc, kr, jp]
    } else {
        [kr, sc, jp]
    };
    chain.extend(order.iter().map(|(noto, _)| *noto));
    chain.extend(["Segoe UI", "Nirmala UI", "Leelawadee UI"]);
    chain.extend(order.iter().flat_map(|(_, system)| system.iter().copied()));
    chain
}

/// The chain reduced to installed families (Noto is skipped when not installed); never empty.
pub fn installed_fonts(language: &str, installed: &dyn Fn(&str) -> bool) -> Vec<String> {
    let fonts: Vec<String> = font_chain(language).into_iter().filter(|f| installed(f)).map(String::from).collect();
    if fonts.is_empty() {
        vec!["Segoe UI".to_string()]
    } else {
        fonts
    }
}

fn font_checker() -> impl Fn(&str) -> bool {
    let collection = unsafe {
        DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED).ok().and_then(|factory| {
            let mut collection = None;
            factory.GetSystemFontCollection(&mut collection, false).ok()?;
            collection
        })
    };
    move |family: &str| {
        let Some(collection) = &collection else { return family == "Segoe UI" };
        let wide: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
        let (mut index, mut exists) = (0u32, false.into());
        unsafe { collection.FindFamilyName(PCWSTR(wide.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool() }
    }
}

// ---- Windows settings --------------------------------------------------------------------------------------

/// `HKCU\...\Themes\Personalize\AppsUseLightTheme`; light when unreadable.
pub fn apps_use_light_theme() -> bool {
    let mut data = 1u32;
    let mut size = 4u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.0 != 0 || data != 0
}

/// `SPI_GETCLIENTAREAANIMATION`: true when Windows plays animations.
pub fn client_area_animation() -> bool {
    let mut on = windows::core::BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_err() || on.as_bool()
}

fn load_theme_json() -> Option<Value> {
    let path = crate::paths::data_dir().join("theme.json");
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > THEME_JSON_MAX_BYTES {
        crate::warn!("theme.json is larger than {} bytes; ignored", THEME_JSON_MAX_BYTES);
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    value.is_object().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env<'a>(dark: bool, installed: &'a dyn Fn(&str) -> bool) -> Env<'a> {
        Env { system_dark: dark, system_reduce_motion: false, custom: None, language: "zh-TW", font_installed: installed }
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn shade_reproduces_the_default_accent_family() {
        // Spec §4.4: 7F77DD shaded -25% is 5F59A6.
        assert_eq!(shade(0xFF7F77DD, -0.25), 0xFF5F59A6);
        assert_eq!(shade(0xFF000000, 0.5), 0xFF808080); // 127.5 rounds to even: 128
        assert_eq!(shade(0x80FFFFFF, -0.5), 0x80808080);
        assert!((luminance(0xFFFFFFFF) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn defaults_are_the_soft_light_palette() {
        let p = compute(&json!({}), &env(false, &all));
        assert!(!p.dark);
        assert_eq!(p.accent, 0xFF7F77DD);
        assert_eq!(p.accent_text, 0xFFFFFFFF);
        assert_eq!(p.surface, 0xFFFFFFFF);
        assert_eq!(p.surface_alt, 0xFFEFEDE6);
        assert_eq!(p.text, 0xFF1F1E1D);
        assert_eq!(p.text_muted, 0xFF6B6A66);
        assert_eq!(p.border, 0x16000000);
        assert_eq!(p.danger, 0xFFC73A39);
        assert_eq!(p.corner_radius, 16.0);
        assert!(!p.reduce_motion);
    }

    #[test]
    fn mode_system_follows_windows_and_explicit_modes_win() {
        let system = json!({ "theme": { "mode": "System" } });
        assert!(compute(&system, &env(true, &all)).dark);
        assert!(!compute(&system, &env(false, &all)).dark);
        assert!(compute(&json!({ "theme": { "mode": "Dark" } }), &env(false, &all)).dark);
        assert!(!compute(&json!({ "theme": { "mode": "light" } }), &env(true, &all)).dark);
        let dark = compute(&json!({ "theme": { "mode": "Dark" } }), &env(false, &all));
        assert_eq!((dark.surface, dark.text, dark.border), (0xFF262624, 0xFFF1EFE8, 0x1CFFFFFF));
    }

    #[test]
    fn accent_presets_override_the_accent_and_pick_by_variant() {
        let peach = |mode: &str| json!({ "theme": { "mode": mode, "presetKey": "peach", "accent": "#112233" } });
        assert_eq!(compute(&peach("Light"), &env(false, &all)).accent, 0xFFE07A70);
        assert_eq!(compute(&peach("Dark"), &env(false, &all)).accent, 0xFFF29B91);
        // An unknown preset falls back to `accent`.
        let unknown = json!({ "theme": { "presetKey": "nope", "accent": "#112233" } });
        assert_eq!(compute(&unknown, &env(false, &all)).accent, 0xFF112233);
        // A bad accent falls back to the default.
        assert_eq!(compute(&json!({ "theme": { "accent": "purple" } }), &env(false, &all)).accent, DEFAULT_ACCENT);
    }

    #[test]
    fn text_on_accent_flips_for_bright_accents() {
        let amber = json!({ "theme": { "mode": "Dark", "presetKey": "amber" } }); // F0BA6A, bright
        assert_eq!(compute(&amber, &env(false, &all)).accent_text, 0xFF1F1E1D);
        let mono_dark = json!({ "theme": { "mode": "Dark", "presetKey": "monochrome" } });
        let p = compute(&mono_dark, &env(false, &all));
        assert_eq!((p.accent, p.accent_text), (0xFFF1EFE8, 0xFF1F1E1D));
        let mono_light = json!({ "theme": { "mode": "Light", "presetKey": "monochrome" } });
        let p = compute(&mono_light, &env(false, &all));
        assert_eq!((p.accent, p.accent_text), (0xFF1F1E1D, 0xFFFFFFFF));
    }

    #[test]
    fn corner_radius_is_clamped_to_0_28() {
        let radius = |v: serde_json::Value| compute(&json!({ "theme": { "cornerRadius": v } }), &env(false, &all)).corner_radius;
        assert_eq!(radius(json!(40)), 28.0);
        assert_eq!(radius(json!(-3)), 0.0);
        assert_eq!(radius(json!(10)), 10.0);
    }

    #[test]
    fn reduce_motion_follows_the_setting_then_windows() {
        let mut e = env(false, &all);
        assert!(!compute(&json!({}), &e).reduce_motion);
        e.system_reduce_motion = true;
        assert!(compute(&json!({}), &e).reduce_motion);
        assert!(compute(&json!({ "theme": { "reduceMotion": null } }), &e).reduce_motion);
        assert!(!compute(&json!({ "theme": { "reduceMotion": false } }), &e).reduce_motion);
        e.system_reduce_motion = false;
        assert!(compute(&json!({ "theme": { "reduceMotion": true } }), &e).reduce_motion);
    }

    #[test]
    fn theme_json_overrides_only_its_own_variant() {
        let custom = json!({
            "dark": { "Surface": "#18222C", "textSecondary": "#A8B0B8", "border": "#1CFFFFFF", "danger": "bad" },
            "Light": { "surface": "#000000" }
        });
        let mut e = env(false, &all);
        e.custom = Some(custom);
        let dark = compute(&json!({ "theme": { "mode": "Dark" } }), &e);
        assert_eq!(dark.surface, 0xFF18222C);
        assert_eq!(dark.text_muted, 0xFFA8B0B8);
        assert_eq!(dark.border, 0x1CFFFFFF);
        assert_eq!(dark.danger, 0xFFF09595); // invalid colour ignored
        // "Light" is not a valid variant name (case-sensitive), so light keeps the built-in colours.
        assert_eq!(compute(&json!({ "theme": { "mode": "Light" } }), &e).surface, 0xFFFFFFFF);
    }

    #[test]
    fn hex_colours_follow_upstreams_rules() {
        assert_eq!(parse_hex("#7F77DD"), Some(0xFF7F77DD));
        assert_eq!(parse_hex("#1C112233"), Some(0x1C112233));
        assert_eq!(parse_hex("7F77DD"), None);
        assert_eq!(parse_hex("#7F77D"), None);
        assert_eq!(parse_hex("#GG77DD"), None);
    }

    #[test]
    fn hsl_round_trips_and_material_you_builds_a_scheme() {
        for c in [0xFF7F77DD_u32, 0xFFE07A70, 0xFF529B7D, 0xFF5B8BE3] {
            let (h, s, l) = rgb_to_hsl(c);
            let back = hsl_to_rgb(h, s, l);
            for (a, b) in [(c >> 16 & 255, back >> 16 & 255), (c >> 8 & 255, back >> 8 & 255), (c & 255, back & 255)] {
                assert!((a as i32 - b as i32).abs() <= 1, "{c:08X} -> {back:08X}");
            }
        }
        let my = |mode: &str, accent: &str| {
            json!({ "theme": { "style": "MaterialYou", "mode": mode, "accent": accent } })
        };
        // Light seed 7F77DD is lighter than 0.60, so primary is re-derived at lightness 0.45.
        let light = compute(&my("Light", "#7F77DD"), &env(false, &all));
        assert_eq!(light.surface, 0xFFFFFFFF);
        assert_ne!(light.accent, 0xFF7F77DD);
        assert!(luminance(light.accent) < 0.55 && light.accent_text == 0xFFFFFFFF);
        assert_eq!(light.border >> 24, 28);
        let dark = compute(&my("Dark", "#7F77DD"), &env(false, &all));
        assert_eq!(dark.border >> 24, 32);
        assert!(luminance(dark.surface) < 0.2);
        // A grey seed gets the fixed monochrome scheme.
        let grey = compute(&my("Light", "#808080"), &env(false, &all));
        assert_eq!((grey.surface_alt, grey.accent, grey.border), (0xFFEEECE5, 0xFF1F1E1D, 0x18000000));
        let grey_dark = compute(&my("Dark", "#808080"), &env(false, &all));
        assert_eq!((grey_dark.surface, grey_dark.accent), (0xFF222220, 0xFFF1EFE8));
    }

    #[test]
    fn zh_tw_uses_noto_then_jhenghei_and_skips_what_is_not_installed() {
        let with_noto = |f: &str| matches!(f, "Noto Sans" | "Microsoft JhengHei UI" | "Segoe UI");
        let fonts = installed_fonts("zh-TW", &with_noto);
        assert_eq!(fonts, ["Noto Sans", "Microsoft JhengHei UI", "Segoe UI"]);
        let without_noto = |f: &str| matches!(f, "Microsoft JhengHei UI" | "Segoe UI" | "Microsoft YaHei");
        let fonts = installed_fonts("zh-TW", &without_noto);
        assert_eq!(fonts[0], "Microsoft JhengHei UI");
        assert!(!fonts.iter().any(|f| f.starts_with("Noto")));
        // Simplified Chinese does not get the JhengHei entries.
        assert!(!font_chain("zh-CN").contains(&"Microsoft JhengHei UI"));
        // Nothing installed still yields a usable family.
        assert_eq!(installed_fonts("en", &|_: &str| false), ["Segoe UI"]);
    }

    #[test]
    fn cjk_order_follows_the_language() {
        let pos = |chain: &[&str], name: &str| chain.iter().position(|f| *f == name).unwrap();
        let ja = font_chain("ja");
        assert!(pos(&ja, "Noto Sans JP") < pos(&ja, "Noto Sans KR") && pos(&ja, "Noto Sans KR") < pos(&ja, "Noto Sans SC"));
        let zh = font_chain("zh-CN");
        assert!(pos(&zh, "Noto Sans SC") < pos(&zh, "Noto Sans KR"));
        let en = font_chain("en");
        assert!(pos(&en, "Noto Sans KR") < pos(&en, "Noto Sans SC"));
    }

    #[test]
    fn windows_probes_return_without_panicking() {
        let _ = apps_use_light_theme();
        let _ = client_area_animation();
        let installed = font_checker();
        assert!(installed("Segoe UI"));
        assert!(!installed("Definitely Not A Font 123"));
    }
}
