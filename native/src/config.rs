//! Your switches, saved as a small JSON file in %APPDATA%\TwoCode.
use crate::agents::Run;
use crate::glow::{Motion, Palette, Style};
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
    pub finish_popup: bool,
    pub hide_fullscreen: bool,
    pub tab: Tab,
    pub sounds: bool,
    pub recent: bool,
    pub calendar: bool,
    pub todos: bool,
    pub shelf: bool,
    pub bluetooth: bool,
    /// Swiping left goes to the tab on the left. Off reverses it.
    pub swipe_follows_fingers: bool,
    /// Windows' name for the chosen display. Empty or gone means the main one.
    pub display: String,
    /// The light around the edges of the screen.
    pub glow: bool,
    /// The glow shows only while something is playing.
    pub glow_only_playing: bool,
    pub glow_palette: Palette,
    /// Your own colors, picked on the wheel. Only the first `glow_custom_count` are used.
    pub glow_custom: [u32; 3],
    pub glow_custom_count: u8,
    pub glow_style: Style,
    pub glow_motion: Motion,
    /// 0 thin, 1 medium, 2 thick.
    pub glow_size: u8,
    /// 0 soft, 1 medium, 2 bright.
    pub glow_strength: u8,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            media: true, lyrics: true, agents: true, track_claude: true, track_codex: true, finish_popup: false, hide_fullscreen: true, tab: Tab::Media, sounds: false, recent: true, calendar: true, todos: true, shelf: true, bluetooth: true, swipe_follows_fingers: true,
            display: String::new(),
            glow: false,
            glow_only_playing: true,
            glow_palette: Palette::Album,
            glow_custom: [0xFF4FA3, 0x7C5CFF, 0x22D3EE],
            glow_custom_count: 2,
            glow_style: Style::Around,
            glow_motion: Motion::Music,
            glow_size: 1,
            glow_strength: 1,
        }
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

pub fn load_runs() -> Vec<Run> {
    fs::read_to_string(dir().join("recent.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn save_runs(runs: &[Run]) {
    let _ = fs::create_dir_all(dir());
    if let Ok(text) = serde_json::to_string(runs) {
        let temp = dir().join("recent.json.tmp");
        if fs::write(&temp, text).is_ok() {
            let _ = fs::rename(temp, dir().join("recent.json"));
        }
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

    #[test]
    fn settings_saved_on_the_old_recent_tab_still_load() {
        let c: Config = serde_json::from_str(r#"{ "media": false, "tab": "Recent" }"#).unwrap();
        assert!(!c.media);
        assert_eq!(c.tab, Tab::Agents);
    }
}
