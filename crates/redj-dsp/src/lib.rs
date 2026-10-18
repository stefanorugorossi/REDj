//! Real-time safe DSP building blocks: EQ, filters, crossfader curves,
//! limiter, interpolation and time-stretch.
//!
//! Every processor allocates in its constructor only; `process` methods never
//! allocate, lock or block, so they can run inside an audio callback.

pub mod biquad;
pub mod echo;
pub mod eq;
pub mod filter;
pub mod interp;
pub mod limiter;
pub mod smooth;
pub mod source;
pub mod stretch;
pub mod xfade;

pub use biquad::Biquad;
pub use echo::Echo;
pub use eq::IsolatorEq;
pub use filter::DjFilter;
pub use interp::SincTable;
pub use limiter::TruePeakLimiter;
pub use smooth::Smoother;
pub use source::Source;
pub use stretch::{TimeStretcher, Wsola};
pub use xfade::{CrossfaderCurve, crossfader_gains};

/// Converts decibels to a linear gain.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}
