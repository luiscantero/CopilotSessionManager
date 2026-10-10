//! Checks the UI translations in `src/locales` against the default language (en-us).

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_LANGUAGE: &str = "en-us";

#[derive(Deserialize)]
struct Language {
    code: String,
    name: String,
    dir: Option<String>,
}

fn frontend_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../src")
}

fn locales_dir() -> PathBuf {
    frontend_dir().join("locales")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("Unable to read {}: {err}", path.display()))
}

fn languages() -> Vec<Language> {
    let path = locales_dir().join("languages.json");
    serde_json::from_str(&read(&path)).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn locale(code: &str) -> BTreeMap<String, String> {
    let path = locales_dir().join(format!("{code}.json"));
    let text = read(&path);
    assert!(
        !text.starts_with('\u{feff}'),
        "{code}.json must not start with a BOM"
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{code}.json must be a flat object of strings: {err}"))
}

fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            found.insert(name);
        }
        rest = &rest[end + 1..];
    }
    found
}

/// String literals passed to the translation helpers in main.js, e.g. `t("list.empty")`.
fn keys_used_in_script(script: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for helper in ["t(\"", "tHtml(\"", "hasTranslation(\""] {
        for (index, _) in script.match_indices(helper) {
            let before = script[..index].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.')) {
                continue;
            }
            let rest = &script[index + helper.len()..];
            let end = rest.find('"').expect("unterminated translation key");
            keys.insert(rest[..end].to_string());
        }
    }
    keys
}

/// Values of `data-i18n` and `data-i18n-<attribute>` attributes in index.html.
fn keys_used_in_html(html: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (index, _) in html.match_indices("data-i18n") {
        let rest = &html[index + "data-i18n".len()..];
        let name_end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(0);
        let Some(value) = rest[name_end..].strip_prefix("=\"") else {
            continue;
        };
        let end = value.find('"').expect("unterminated data-i18n attribute");
        keys.insert(value[..end].to_string());
    }
    keys
}

#[test]
fn manifest_lists_exactly_the_locale_files() {
    let languages = languages();
    let listed: BTreeSet<String> = languages
        .iter()
        .map(|language| language.code.clone())
        .collect();
    assert_eq!(
        listed.len(),
        languages.len(),
        "languages.json has duplicate codes"
    );
    assert!(
        listed.contains(DEFAULT_LANGUAGE),
        "languages.json must list {DEFAULT_LANGUAGE}"
    );

    let files: BTreeSet<String> = fs::read_dir(locales_dir())
        .expect("locales folder")
        .map(|entry| {
            entry
                .expect("locale file")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "languages.json")
        .map(|name| {
            name.strip_suffix(".json")
                .unwrap_or_else(|| panic!("Unexpected file {name} in src/locales"))
                .to_string()
        })
        .collect();
    assert_eq!(listed, files, "languages.json and the locale files differ");

    for language in &languages {
        assert_eq!(
            language.code,
            language.code.to_lowercase(),
            "codes are lowercase: {}",
            language.code
        );
        assert!(
            !language.name.trim().is_empty(),
            "{} has no display name",
            language.code
        );
        assert!(
            matches!(language.dir.as_deref(), None | Some("ltr" | "rtl")),
            "{} has an invalid dir",
            language.code
        );
    }
}

#[test]
fn expected_languages_are_available() {
    let listed: BTreeSet<String> = languages()
        .into_iter()
        .map(|language| language.code)
        .collect();
    for code in [
        "en-us", "de-de", "fr-fr", "es-es", "it-it", "zh-cn", "ja-jp", "ko-kr", "sv-se", "pt-br",
        "he-il", "ar", "vi-vn", "th-th",
    ] {
        assert!(listed.contains(code), "{code} is missing");
    }
}

#[test]
fn every_language_translates_all_default_keys() {
    let default = locale(DEFAULT_LANGUAGE);
    assert!(!default.is_empty(), "{DEFAULT_LANGUAGE}.json is empty");

    let mut problems = Vec::new();
    for language in languages() {
        let code = &language.code;
        let strings = locale(code);
        for key in default.keys().filter(|key| !strings.contains_key(*key)) {
            problems.push(format!("{code}: missing \"{key}\""));
        }
        for key in strings.keys().filter(|key| !default.contains_key(*key)) {
            problems.push(format!("{code}: unknown key \"{key}\""));
        }
        for (key, value) in &strings {
            if value.trim().is_empty() {
                problems.push(format!("{code}: \"{key}\" is empty"));
            }
            if let Some(original) = default.get(key) {
                if placeholders(value) != placeholders(original) {
                    problems.push(format!(
                        "{code}: \"{key}\" has placeholders {:?}, expected {:?}",
                        placeholders(value),
                        placeholders(original)
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "Translation problems:\n{}",
        problems.join("\n")
    );
}

#[test]
fn frontend_uses_only_known_keys_and_every_key_is_used() {
    let default = locale(DEFAULT_LANGUAGE);
    let mut used = keys_used_in_script(&read(&frontend_dir().join("main.js")));
    used.extend(keys_used_in_html(&read(&frontend_dir().join("index.html"))));

    let unknown: Vec<_> = used
        .iter()
        .filter(|key| !default.contains_key(*key))
        .collect();
    assert!(
        unknown.is_empty(),
        "Keys missing from {DEFAULT_LANGUAGE}.json: {unknown:?}"
    );
    let unused: Vec<_> = default.keys().filter(|key| !used.contains(*key)).collect();
    assert!(
        unused.is_empty(),
        "Unused keys in {DEFAULT_LANGUAGE}.json: {unused:?}"
    );
}

#[test]
fn key_and_placeholder_parsing() {
    assert_eq!(
        placeholders("{shown} of {total}"),
        BTreeSet::from(["shown", "total"])
    );
    assert!(placeholders("no { placeholders } here").is_empty());
    assert_eq!(
        keys_used_in_script(r#"x = t("a.b") + cat("no") + tHtml("c");"#),
        BTreeSet::from(["a.b".to_string(), "c".to_string()])
    );
    assert_eq!(
        keys_used_in_html(r#"<b data-i18n="a" data-i18n-title="b" data-icon="x">"#),
        BTreeSet::from(["a".to_string(), "b".to_string()])
    );
}
