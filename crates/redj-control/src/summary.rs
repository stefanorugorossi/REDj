//! Compact, model-friendly views of the library.
//!
//! Slow local models pay for every token, so summaries are small, numbers
//! are rounded, and the useful derived facts (where a track can be mixed in
//! and out) are computed here instead of being left to the model.

use redj_core::{Key, SectionKind, TrackInfo};
use serde::Serialize;

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Bars where a track can be mixed in: `[start, end)` of its intro.
pub fn mix_in_range(t: &TrackInfo) -> (f64, f64) {
    match t.sections.iter().find(|s| s.kind == SectionKind::Intro) {
        Some(s) if s.start_bar < 8.0 => (s.start_bar.max(0.0), s.end_bar),
        _ => (0.0, 16.0f64.min(t.end_bar().floor())),
    }
}

/// Bars where a track can be mixed out: `[start, end)` of its outro (or the
/// last 16 bars).
pub fn mix_out_range(t: &TrackInfo) -> (f64, f64) {
    let end = t.end_bar().floor();
    match t
        .sections
        .iter()
        .rev()
        .find(|s| s.kind == SectionKind::Outro)
    {
        Some(s) => (s.start_bar, s.end_bar.min(end)),
        None => (((end - 16.0) / 8.0).floor().max(0.0) * 8.0, end),
    }
}

/// One line per track for library listings.
#[derive(Debug, Clone, Serialize)]
pub struct TrackSummary {
    pub id: String,
    pub name: String,
    pub bpm: f64,
    /// Camelot code, e.g. "8A".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Bars from bar 0 to the end of the audio.
    pub bars: f64,
    pub minutes: f64,
    /// Mean energy 0..1.
    pub energy: f64,
    /// Intro bars `[start, end)`: a good place to start this track.
    pub mix_in: (f64, f64),
    /// Outro bars `[start, end)`: a good place to mix out of this track.
    pub mix_out: (f64, f64),
}

impl TrackSummary {
    pub fn new(t: &TrackInfo) -> Self {
        let e = if t.bar_energy.is_empty() {
            0.0
        } else {
            t.bar_energy.iter().map(|&x| f64::from(x)).sum::<f64>() / t.bar_energy.len() as f64
        };
        Self {
            id: t.id.0.clone(),
            name: t.display_name(),
            bpm: r2(t.grid.bpm),
            key: t.key.map(|k| k.camelot()),
            bars: r1(t.end_bar()),
            minutes: r2(t.duration_sec / 60.0),
            energy: r2(e),
            mix_in: mix_in_range(t),
            mix_out: mix_out_range(t),
        }
    }
}

/// A section as shown to the model.
#[derive(Debug, Clone, Serialize)]
pub struct SectionView {
    pub kind: SectionKind,
    pub start_bar: f64,
    pub end_bar: f64,
    pub energy: f64,
}

/// Full analysis of one track, still compact.
#[derive(Debug, Clone, Serialize)]
pub struct TrackDetail {
    #[serde(flatten)]
    pub summary: TrackSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_confidence: Option<f64>,
    pub loudness_lufs: f64,
    pub constant_tempo: bool,
    pub sections: Vec<SectionView>,
    /// Mean energy of each 8-bar phrase, from bar 0.
    pub phrase_energy: Vec<f64>,
}

impl TrackDetail {
    pub fn new(t: &TrackInfo) -> Self {
        let phrase_energy = t
            .bar_energy
            .chunks(8)
            .map(|c| r2(c.iter().map(|&x| f64::from(x)).sum::<f64>() / c.len() as f64))
            .collect();
        Self {
            summary: TrackSummary::new(t),
            key_name: t.key.map(|k| k.to_string()),
            key_confidence: t.key.map(|k| r2(f64::from(k.confidence))),
            loudness_lufs: r1(t.loudness.integrated_lufs),
            constant_tempo: t.grid.is_constant(),
            sections: t
                .sections
                .iter()
                .map(|s| SectionView {
                    kind: s.kind,
                    start_bar: s.start_bar,
                    end_bar: s.end_bar,
                    energy: r2(f64::from(s.energy)),
                })
                .collect(),
            phrase_energy,
        }
    }
}

/// How well track `to` follows track `from`.
#[derive(Debug, Clone, Serialize)]
pub struct Compatibility {
    pub from: String,
    pub to: String,
    /// Tempo change needed, in percent of `from`'s tempo.
    pub tempo_change_pct: f64,
    /// Camelot wheel distance (0 = same key, 1 = adjacent, >2 = clash).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_distance: Option<u8>,
    /// Energy of `to` minus energy of `from`.
    pub energy_delta: f64,
    /// 0..1, higher is a smoother match.
    pub score: f64,
}

/// Camelot distance between two optional keys.
pub fn key_distance(a: Option<Key>, b: Option<Key>) -> Option<u8> {
    Some(a?.camelot_distance(&b?))
}

impl Compatibility {
    pub fn new(a: &TrackInfo, b: &TrackInfo) -> Self {
        let sa = TrackSummary::new(a);
        let sb = TrackSummary::new(b);
        let tempo = (b.grid.bpm / a.grid.bpm - 1.0) * 100.0;
        let kd = key_distance(a.key, b.key);
        let tempo_score = (1.0 - tempo.abs() / 8.0).max(0.0);
        let key_score = match kd {
            Some(d) => (1.0 - f64::from(d) / 4.0).max(0.0),
            None => 0.5,
        };
        Self {
            from: sa.id,
            to: sb.id,
            tempo_change_pct: r1(tempo),
            key_distance: kd,
            energy_delta: r2(sb.energy - sa.energy),
            score: r2(0.6 * tempo_score + 0.4 * key_score),
        }
    }
}
