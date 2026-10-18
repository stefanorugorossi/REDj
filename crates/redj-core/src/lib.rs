//! Shared units and types: beats, bars, sample positions, ids, track info.
//!
//! Units are plain `f64`/`u64` aliases rather than newtypes: they travel through
//! JSON plans written by humans and language models, and the field names carry
//! the unit (`*_beat`, `*_bar`, `*_sec`, `*_db`).

mod grid;
mod key;
mod library;
mod track;

pub use grid::{BeatAnchor, BeatGrid};
pub use key::{Key, Mode};
pub use library::Library;
pub use track::{Loudness, Section, SectionKind, TrackId, TrackInfo, TrackTags};

/// Position or length in beats (quarter notes for 4/4 material).
pub type Beats = f64;
/// Position or length in bars.
pub type Bars = f64;
/// Time in seconds.
pub type Seconds = f64;
/// Absolute sample frame index (one frame = one sample per channel).
pub type SamplePos = u64;

/// Beats per bar. REDj targets 4/4 material; the constant keeps the
/// assumption visible instead of scattering literal fours.
pub const BEATS_PER_BAR: u32 = 4;

/// Converts bars to beats.
#[inline]
pub fn bars_to_beats(bars: Bars) -> Beats {
    bars * f64::from(BEATS_PER_BAR)
}

/// Converts beats to bars.
#[inline]
pub fn beats_to_bars(beats: Beats) -> Bars {
    beats / f64::from(BEATS_PER_BAR)
}

/// Converts decibels to a linear gain factor.
#[inline]
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Converts a linear gain factor to decibels (`-inf` for zero).
#[inline]
pub fn gain_to_db(gain: f64) -> f64 {
    20.0 * gain.log10()
}
