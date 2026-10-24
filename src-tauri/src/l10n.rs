//! Native translations (rust-i18n).
//!
//! Word lists live in `src-tauri/locales/<locale>.json` and are embedded at
//! compile time by [`rust_i18n::i18n!`] in `lib.rs`. Values are addressed with
//! `native.*` keys from Rust code only; the webview word lists under
//! `ui/locales/` are a separate set (i18next) and the two must not overlap.
//!
//! `zh-CN` is the source language **and** the fallback: a missing key shows the
//! Chinese string instead of a raw key. `en-US` is kept in sync, and the tests at
//! the bottom of this file enforce that mechanically (same keys, no empty values,
//! no CJK left in the English column).

/// Switch the language used by [`rust_i18n::t!`] for the whole process.
///
/// Called once at startup *before* the tray is built, and again whenever the
/// user changes the language setting (the tray menu is rebuilt from the caller).
pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(locale);
}

/// Locale currently used by native translations.
pub fn locale() -> String {
    rust_i18n::locale().to_string()
}

/// Locales the host ships native translations for (sorted).
pub fn available_locales() -> Vec<String> {
    let locales = rust_i18n::available_locales!();
    let mut out: Vec<String> = locales.iter().map(|l| l.to_string()).collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    const ZH: &str = include_str!("../locales/zh-CN.json");
    const EN: &str = include_str!("../locales/en-US.json");

    /// Flatten `{ "a": { "b": "text" } }` into `{"a.b": "text"}`.
    fn flatten(value: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    flatten(child, &format!("{prefix}{key}."), out);
                }
            }
            Value::String(text) => {
                out.insert(prefix.trim_end_matches('.').to_string(), text.clone());
            }
            other => panic!("only strings are allowed in a word list, found {other:?} at {prefix}"),
        }
    }

    fn load(raw: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let value: Value = serde_json::from_str(raw).expect("word list is valid JSON");
        flatten(&value, "", &mut out);
        out
    }

    fn has_cjk(text: &str) -> bool {
        text.chars().any(|c| {
            let code = c as u32;
            // CJK ideographs, CJK punctuation and full-width forms.
            (0x3000..=0x303F).contains(&code)
                || (0x4E00..=0x9FFF).contains(&code)
                || (0xF900..=0xFAFF).contains(&code)
                || (0xFF00..=0xFFEF).contains(&code)
        })
    }

    #[test]
    fn wordlists_have_the_same_keys() {
        let zh = load(ZH);
        let en = load(EN);
        let missing_in_en: Vec<&String> = zh.keys().filter(|k| !en.contains_key(*k)).collect();
        let missing_in_zh: Vec<&String> = en.keys().filter(|k| !zh.contains_key(*k)).collect();
        assert!(
            missing_in_en.is_empty(),
            "en-US is missing: {missing_in_en:?}"
        );
        assert!(
            missing_in_zh.is_empty(),
            "zh-CN is missing: {missing_in_zh:?}"
        );
    }

    #[test]
    fn wordlists_have_no_empty_values() {
        for (name, raw) in [("zh-CN", ZH), ("en-US", EN)] {
            for (key, value) in load(raw) {
                assert!(
                    !value.trim().is_empty(),
                    "{name}.json has an empty value for {key}"
                );
            }
        }
    }

    #[test]
    fn english_wordlist_contains_no_cjk() {
        for (key, value) in load(EN) {
            assert!(
                !has_cjk(&value),
                "en-US value for {key} still contains CJK: {value:?}"
            );
        }
    }

    #[test]
    fn translations_resolve_from_the_embedded_wordlists() {
        // Pass the locale explicitly: `set_locale` is process-wide and these tests
        // run in parallel with the install/registry ones, which assert Chinese text.
        assert_eq!(
            rust_i18n::t!("native.menu.panel", locale = "zh-CN"),
            "打开工具面板"
        );
        assert_eq!(
            rust_i18n::t!("native.menu.panel", locale = "en-US"),
            "Open panel"
        );
        assert_eq!(
            rust_i18n::t!("native.action.reason.conflict", locale = "en-US", owner = "X"),
            "Conflicts with “X”"
        );
        assert_eq!(
            rust_i18n::t!("native.install.entry_missing", locale = "zh-CN", entry = "a.html"),
            "入口文件不存在：a.html"
        );
    }

    #[test]
    fn permission_wordlists_cover_every_permission() {
        let zh = load(ZH);
        for key in [
            "files.open",
            "files.save",
            "clipboard.read",
            "clipboard.write",
            "data",
            "external.open",
            "network",
        ] {
            let base = key.replace('.', "_");
            assert!(
                zh.contains_key(&format!("native.permission.{base}.label")),
                "no label for {key}"
            );
            assert!(
                zh.contains_key(&format!("native.permission.{base}.detail")),
                "no detail for {key}"
            );
        }
    }
}
