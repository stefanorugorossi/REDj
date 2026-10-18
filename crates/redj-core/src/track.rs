use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Bars, BeatGrid, Key, Seconds, bars_to_beats};

/// Stable track identifier: the first 16 hex digits of the BLAKE3 hash of
/// the file content. Renaming or moving a file keeps its id.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct TrackId(pub String);

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for TrackId {
    fn from(s: &str) -> Self {
        TrackId(s.to_owned())
    }
}

/// Tag metadata read from the file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackTags {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// BPM written in the tags, if any (not the analyzed tempo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
}

/// Structural section kind of an EDM track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// Opening part, usually drums only: good to mix in over.
    Intro,
    /// Reduced-energy part without kick drum.
    Breakdown,
    /// Rising tension right before a drop.
    Buildup,
    /// Full-energy main part.
    Drop,
    /// Closing part, usually drums only: good to mix out over.
    Outro,
    /// Anything else (verse-like body sections).
    Body,
}

/// A section of a track, aligned to bar boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Section {
    pub kind: SectionKind,
    /// First bar of the section (bar 0 = first downbeat).
    pub start_bar: Bars,
    /// Bar where the section ends (exclusive).
    pub end_bar: Bars,
    /// Mean energy of the section, 0..1 relative to the track.
    pub energy: f32,
}

/// Loudness measurements (EBU R128).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Loudness {
    /// Integrated loudness in LUFS.
    pub integrated_lufs: f64,
    /// Maximum true peak in dBTP.
    pub true_peak_dbtp: f64,
    /// Loudness range in LU.
    pub range_lu: f64,
}

/// Everything REDj knows about one track after analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackInfo {
    pub id: TrackId,
    pub path: PathBuf,
    #[serde(default)]
    pub tags: TrackTags,
    pub duration_sec: Seconds,
    pub sample_rate: u32,
    pub channels: u16,
    pub grid: BeatGrid,
    /// Sections in order, covering the track from bar 0 to `end_bar()`.
    pub sections: Vec<Section>,
    /// Energy per bar (index = bar number), 0..1 relative to the track.
    pub bar_energy: Vec<f32>,
    pub loudness: Loudness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Key>,
    /// Version of the analyzer that produced this record.
    pub analyzer_version: u32,
}

impl TrackInfo {
    /// Display name: "Artist - Title" or the file stem.
    pub fn display_name(&self) -> String {
        match (&self.tags.artist, &self.tags.title) {
            (Some(a), Some(t)) => format!("{a} - {t}"),
            (None, Some(t)) => t.clone(),
            _ => self
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.id.0.clone()),
        }
    }

    /// Bar position of the end of the audio.
    pub fn end_bar(&self) -> Bars {
        crate::beats_to_bars(self.grid.sec_to_beat(self.duration_sec))
    }

    /// Time of the start of a bar.
    pub fn bar_to_sec(&self, bar: Bars) -> Seconds {
        self.grid.beat_to_sec(bars_to_beats(bar))
    }

    /// Section containing the given bar, if any.
    pub fn section_at(&self, bar: Bars) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| bar >= s.start_bar && bar < s.end_bar)
    }
}
