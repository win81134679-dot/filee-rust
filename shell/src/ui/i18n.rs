//! Translations: upstream's `i18n\<lang>.json` (flat `{"key": "text"}`), looked up current language -> English -> key.
//! Placeholders are `{0}`, `{1}` … (spec 03 §5).

use std::collections::HashMap;
use std::sync::RwLock;

use windows::core::PWSTR;
use windows::Win32::Globalization::{GetUserDefaultLocaleName, GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

/// The 17 languages upstream ships, in the order of its language menu.
pub const LANGUAGES: [&str; 17] =
    ["en", "de", "tr", "es", "ja", "ko", "zh-CN", "zh-TW", "pl", "hi", "ru", "ar", "pt-PT", "pt-BR", "fr", "ne", "th"];

struct State {
    lang: String,
    current: HashMap<String, String>,
    english: HashMap<String, String>,
}

impl State {
    fn get(&self, key: &str) -> String {
        self.current.get(key).or_else(|| self.english.get(key)).cloned().unwrap_or_else(|| key.to_string())
    }
}

static STATE: RwLock<Option<State>> = RwLock::new(None);

/// Loads the strings for `language` (a setting value: `auto` or one of [`LANGUAGES`]).
pub fn init(language: &str) {
    let lang = resolve_language(language, &os_ui_language());
    let english = load_strings("en");
    let current = if lang == "en" { HashMap::new() } else { load_strings(&lang) };
    crate::info!("Language {lang} ({} strings)", current.len().max(english.len()));
    *STATE.write().unwrap_or_else(|e| e.into_inner()) = Some(State { lang, current, english });
}

/// Code of the language in use (`zh-TW` …); `en` before [`init`].
pub fn current() -> String {
    STATE.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.lang.clone()).unwrap_or_else(|| "en".into())
}

/// The text for `key`: current language, then English, then the key itself.
pub fn t(key: &str) -> String {
    let guard = STATE.read().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(s) => s.get(key),
        None => key.to_string(),
    }
}

/// [`t`] with `{0}`, `{1}` … replaced by `args`.
pub fn format(key: &str, args: &[&str]) -> String {
    fill(&t(key), args)
}

fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        match tail.find('}').map(|c| (c, tail[..c].parse::<usize>())) {
            Some((close, Ok(n))) if n < args.len() => {
                out.push_str(args[n]);
                rest = &tail[close + 1..];
            }
            _ => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn load_strings(lang: &str) -> HashMap<String, String> {
    let path = crate::paths::i18n_dir().join(format!("{lang}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        crate::warn!("No translations at {}", path.display());
        return HashMap::new();
    };
    parse_strings(&text)
}

fn parse_strings(text: &str) -> HashMap<String, String> {
    let text = text.trim_start_matches('\u{feff}');
    match serde_json::from_str::<HashMap<String, serde_json::Value>>(text) {
        Ok(map) => map.into_iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string()))).collect(),
        Err(e) => {
            crate::warn!("Translations are not valid JSON: {e}");
            HashMap::new()
        }
    }
}

/// The setting value, or for `auto` the Windows display language, mapped onto one of [`LANGUAGES`].
pub fn resolve_language(setting: &str, os_language: &str) -> String {
    if let Some(code) = LANGUAGES.iter().find(|c| c.eq_ignore_ascii_case(setting.trim())) {
        return (*code).to_string();
    }
    map_os_language(os_language).to_string()
}

/// `zh` plus Hant/TW/HK/MO/CHT and no Hans (upstream's `AppLanguages.IsTraditionalChinese`).
pub fn is_traditional_chinese(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("zh")
        && !n.contains("hans")
        && ["hant", "tw", "hk", "mo", "cht"].iter().any(|m| n.contains(m))
}

/// Upstream's `auto` rules for an OS culture name such as `zh-Hant-TW` or `pt-BR`.
pub fn map_os_language(name: &str) -> &'static str {
    let n = name.trim().to_ascii_lowercase();
    let exact_or_dash = |code: &str| n == code || n.starts_with(&format!("{code}-"));
    if exact_or_dash("de") {
        "de"
    } else if n.starts_with("pt") {
        if n == "ptbr" || n.ends_with("-br") {
            "pt-BR"
        } else {
            "pt-PT"
        }
    } else if exact_or_dash("tr") {
        "tr"
    } else if n.starts_with("ja") {
        "ja"
    } else if n.starts_with("ko") {
        "ko"
    } else if n.starts_with("zh") {
        if is_traditional_chinese(&n) {
            "zh-TW"
        } else {
            "zh-CN"
        }
    } else if n.starts_with("pl") {
        "pl"
    } else if n.starts_with("es") {
        "es"
    } else if n.starts_with("hi") {
        "hi"
    } else if n.starts_with("ru") {
        "ru"
    } else if exact_or_dash("ar") {
        "ar"
    } else if exact_or_dash("fr") {
        "fr"
    } else if exact_or_dash("ne") {
        "ne"
    } else if exact_or_dash("th") {
        "th"
    } else {
        "en"
    }
}

/// The Windows display language (first preferred UI language, else the user locale), `""` when unknown.
fn os_ui_language() -> String {
    unsafe {
        let mut count = 0u32;
        let mut len = 0u32;
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_ok() && len > 1 {
            let mut buf = vec![0u16; len as usize];
            if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, Some(PWSTR(buf.as_mut_ptr())), &mut len)
                .is_ok()
            {
                // A multi-string: names separated by NUL, the first one is the preferred language.
                let first: Vec<u16> = buf.iter().copied().take_while(|c| *c != 0).collect();
                if !first.is_empty() {
                    return String::from_utf16_lossy(&first);
                }
            }
        }
        let mut name = [0u16; 85];
        let n = GetUserDefaultLocaleName(&mut name);
        if n > 1 {
            return String::from_utf16_lossy(&name[..(n as usize - 1)]);
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zh_tw_file() -> HashMap<String, String> {
        let path = crate::paths::i18n_dir().join("zh-TW.json");
        parse_strings(&std::fs::read_to_string(path).expect("upstream zh-TW.json"))
    }

    #[test]
    fn zh_tw_strings_match_the_spec_table() {
        let zh = zh_tw_file();
        assert_eq!(zh["toast.done"], "完成");
        assert_eq!(zh["tray.quit"], "結束");
        assert_eq!(zh["tray.open"], "開啟 Filee");
        assert_eq!(zh["toast.done_errors"], "{1} 個中有 {0} 個失敗");
        assert!(zh.len() > 1000);
    }

    #[test]
    fn lookup_falls_back_to_english_then_key() {
        let state = State {
            lang: "zh-TW".into(),
            current: HashMap::from([("a".to_string(), "甲".to_string())]),
            english: HashMap::from([("a".to_string(), "A".to_string()), ("b".to_string(), "B".to_string())]),
        };
        assert_eq!(state.get("a"), "甲");
        assert_eq!(state.get("b"), "B");
        assert_eq!(state.get("nope.key"), "nope.key");
    }

    #[test]
    fn placeholders_are_filled_by_index() {
        assert_eq!(fill("{1} 個中有 {0} 個失敗", &["1", "3"]), "3 個中有 1 個失敗");
        assert_eq!(fill("{0}/s · {1}", &["2 MB", "9 MB"]), "2 MB/s · 9 MB");
        assert_eq!(fill("no placeholder {x} {5}", &["a"]), "no placeholder {x} {5}");
        assert_eq!(fill("{0}{0}", &["ab"]), "abab");
    }

    #[test]
    fn auto_maps_the_windows_language() {
        assert_eq!(resolve_language("auto", "zh-TW"), "zh-TW");
        assert_eq!(resolve_language("auto", "zh-Hant-HK"), "zh-TW");
        assert_eq!(resolve_language("auto", "zh-MO"), "zh-TW");
        assert_eq!(resolve_language("auto", "zh-CN"), "zh-CN");
        assert_eq!(resolve_language("auto", "zh-Hans-TW"), "zh-CN");
        assert_eq!(resolve_language("auto", "zh"), "zh-CN");
        assert_eq!(resolve_language("auto", "pt-BR"), "pt-BR");
        assert_eq!(resolve_language("auto", "pt-Latn-BR"), "pt-BR");
        assert_eq!(resolve_language("auto", "pt-PT"), "pt-PT");
        assert_eq!(resolve_language("auto", "ja-JP"), "ja");
        assert_eq!(resolve_language("auto", "de-AT"), "de");
        assert_eq!(resolve_language("auto", "sv-SE"), "en");
        assert_eq!(resolve_language("auto", ""), "en");
    }

    #[test]
    fn an_explicit_setting_wins_and_ignores_case() {
        assert_eq!(resolve_language("ko", "zh-TW"), "ko");
        assert_eq!(resolve_language("ZH-tw", "en-US"), "zh-TW");
        assert_eq!(resolve_language("klingon", "fr-FR"), "fr");
    }

    #[test]
    fn every_shipped_language_loads() {
        for code in LANGUAGES {
            let strings = load_strings(code);
            assert!(strings.len() > 1000, "{code} has {} strings", strings.len());
        }
    }
}
