//! Block-based mix engine with sample-accurate scheduler and offline and
//! real-time drivers.
//!
//! The same graph runs everywhere: [`PlanPlayer`] evaluates a compiled
//! [`redj_plan::Timeline`] per sample (positions, loops and automation are
//! functions of the master beat, so every event lands on its exact sample),
//! [`Strip`] is the per-deck channel strip, [`Master`] sums and limits.
//! The offline renderer and the live audio callback both just call
//! `process` on fixed-size blocks.

mod audio;
#[cfg(feature = "rt")]
pub mod driver;
mod live;
mod master;
mod player;
mod reader;
mod strip;

pub use audio::TrackAudio;
pub use live::{
    Command, DeckParam, DeckStatus, EngineHandle, Garbage, HOT_CUES, LiveConfig, LiveEngine,
    MAX_DECKS, Status, XfSide,
};
pub use master::Master;
pub use player::PlanPlayer;
pub use reader::DeckReader;
pub use strip::{Strip, StripParams};

/// Engine configuration.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    pub sample_rate: u32,
    /// Frames per processing block.
    pub block_size: usize,
    /// Master ceiling in dBTP.
    pub ceiling_db: f32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44_100,
            block_size: 512,
            ceiling_db: -1.0,
        }
    }
}
