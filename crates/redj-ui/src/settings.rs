//! Persistent user settings.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Output device id (`None` = system default).
    pub audio_device: Option<String>,
    /// Master output channels (0-based).
    pub master_channels: (u16, u16),
    /// Headphone output channels (0-based), if any.
    pub phones_channels: Option<(u16, u16)>,
    /// MIDI input port name (`None` = auto-detect a known controller).
    pub midi_port: Option<String>,
    pub music_dirs: Vec<PathBuf>,
    pub decks: usize,
    /// Visible seconds in the zoomed waveforms.
    pub zoom_sec: f32,
    /// Ollama model for the live AI.
    pub ai_model: String,
    pub ollama_url: String,
    /// Bars before a transition by which the AI must have planned it.
    pub ai_deadline_bars: f64,
    /// Bars of the near future the AI may not change.
    pub frozen_bars: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_device: None,
            master_channels: (0, 1),
            phones_channels: None,
            midi_port: None,
            music_dirs: Vec::new(),
            decks: 2,
            zoom_sec: 8.0,
            ai_model: "qwen3.6:35b-a3b-q4_K_M".into(),
            ollama_url: "http://127.0.0.1:11434".into(),
            ai_deadline_bars: 16.0,
            frozen_bars: 4.0,
        }
    }
}

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "REDj", "redj")
        .map(|d| d.config_dir().join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }
}
