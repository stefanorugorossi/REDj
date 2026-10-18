//! Session core: library, plans, parameter ownership (human vs AI), and the
//! fallback autopilot.

pub mod autoplan;
mod session;
pub mod summary;

pub use autoplan::{AutoPlanOptions, auto_plan};
pub use session::{PlanReport, Session, SessionEvent};
pub use summary::{Compatibility, TrackDetail, TrackSummary};
pub mod console;
pub mod waveform;

pub use console::{Console, LoadedTrack};
