//! Your switches, saved as a small JSON file in %APPDATA%\TwoCode.
use crate::layout::Tab;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    pub media: bool,
    pub lyrics: bool,
    pub agents: bool,
    pub track_claude: bool,
    pub track_codex: bool,
    pub finish_alerts: bool,
    pub hide_fullscreen: bool,
    pub tab: Tab,
}

impl Default for Config {
    fn default() -> Self {
        Config { media: true, lyrics: true, agents: true, track_claude: true, track_codex: true, finish_alerts: true, hide_fullscreen: true, tab: Tab::Media }
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("TwoCode")
}

/// Problems worth finding later. Kept short: it starts over at 64 KB.
pub fn log(text: &str) {
    let _ = fs::create_dir_all(dir());
    let file = dir().join("twocode.log");
    if fs::metadata(&file).is_ok_and(|m| m.len() > 64 * 1024) {
        let _ = fs::remove_file(&file);
    }
    if let Ok(mut out) = fs::OpenOptions::new().create(true).append(true).open(file) {
        use std::io::Write;
        let _ = writeln!(out, "{} {text}", crate::agents::now_ms());
    }
}

impl Config {
    pub fn load() -> Config {
        fs::read_to_string(dir().join("config.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(dir());
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(dir().join("config.json"), text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_fall_back_to_defaults() {
        let c: Config = serde_json::from_str(r#"{ "media": false, "something_old": 1 }"#).unwrap();
        assert!(!c.media);
        assert!(c.agents);
    }
}
