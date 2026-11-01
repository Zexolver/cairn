//! User settings (mirrors the basics of about:preferences) plus site→container rules.
use crate::container::ContainerId;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        }
    }
    pub fn index(self) -> i32 {
        match self {
            Theme::System => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchEngine {
    pub id: &'static str,
    pub name: &'static str,
    pub template: &'static str,
}

pub const SEARCH_ENGINES: &[SearchEngine] = &[
    SearchEngine { id: "brave", name: "Brave Search", template: "https://search.brave.com/search?q=%s" },
    SearchEngine { id: "ddg", name: "DuckDuckGo", template: "https://duckduckgo.com/?q=%s" },
    SearchEngine { id: "ddg-lite", name: "DuckDuckGo Lite", template: "https://lite.duckduckgo.com/lite/?q=%s" },
];

pub fn engine_by_id(id: &str) -> &'static SearchEngine {
    SEARCH_ENGINES.iter().find(|e| e.id == id).unwrap_or(&SEARCH_ENGINES[0])
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SiteRule {
    /// Host such as `example.com`; also matches subdomains.
    pub host: String,
    pub container: ContainerId,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub search_engine: String,
    pub homepage: String,
    pub block_ads: bool,
    pub show_shield_count: bool,
    pub save_history: bool,
    pub site_rules: Vec<SiteRule>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            search_engine: "brave".into(),
            homepage: "about:blank".into(),
            block_ads: true,
            show_shield_count: true,
            save_history: true,
            site_rules: vec![],
        }
    }
}

pub fn host_of(url: &str) -> Option<String> {
    let h = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    Some(h.trim_start_matches("www.").to_string())
}

impl Settings {
    pub fn search_template(&self) -> &'static str {
        engine_by_id(&self.search_engine).template
    }

    /// Container a URL should open in, if a rule matches (longest host wins).
    pub fn container_for(&self, url: &str) -> Option<ContainerId> {
        let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
        self.site_rules
            .iter()
            .filter(|r| host == r.host || host.ends_with(&format!(".{}", r.host)))
            .max_by_key(|r| r.host.len())
            .map(|r| r.container)
    }

    pub fn set_rule(&mut self, host: &str, container: ContainerId) {
        let host = host.trim().trim_start_matches("www.").to_ascii_lowercase();
        if host.is_empty() {
            return;
        }
        self.site_rules.retain(|r| r.host != host);
        self.site_rules.push(SiteRule { host, container });
    }

    pub fn remove_rule(&mut self, host: &str) {
        self.site_rules.retain(|r| r.host != host);
    }

    pub fn forget_container(&mut self, container: ContainerId) {
        self.site_rules.retain(|r| r.container != container);
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_brave_and_system() {
        let s = Settings::default();
        assert_eq!(s.theme, Theme::System);
        assert!(s.search_template().contains("search.brave.com"));
        assert_eq!(engine_by_id("nope").id, "brave");
    }

    #[test]
    fn rules_match_subdomains_longest_wins() {
        let mut s = Settings::default();
        s.set_rule("www.Example.com", 1);
        s.set_rule("mail.example.com", 2);
        assert_eq!(s.container_for("https://example.com/x"), Some(1));
        assert_eq!(s.container_for("https://a.example.com/"), Some(1));
        assert_eq!(s.container_for("https://mail.example.com/"), Some(2));
        assert_eq!(s.container_for("https://notexample.com/"), None);
        s.forget_container(1);
        assert_eq!(s.container_for("https://example.com/"), None);
    }

    #[test]
    fn roundtrip_and_partial_files() {
        let d = std::env::temp_dir().join(format!("cairn-set-{}", std::process::id()));
        let p = d.join("settings.json");
        let mut s = Settings::default();
        s.theme = Theme::Dark;
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).theme, Theme::Dark);
        std::fs::write(&p, r#"{"theme":"light"}"#).unwrap();
        let l = Settings::load(&p);
        assert_eq!(l.theme, Theme::Light);
        assert_eq!(l.search_engine, "brave");
        let _ = std::fs::remove_dir_all(d);
    }
}
