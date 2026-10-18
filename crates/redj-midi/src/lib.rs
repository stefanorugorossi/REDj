//! MIDI controller input/output with mapping profiles, MIDI learn and LED
//! feedback.
//!
//! The mapping layer only sees [`MidiMessage`]s, so tests (and a virtual
//! controller) can inject messages on every OS; midir has no virtual ports
//! on Windows.

mod action;
#[cfg(feature = "io")]
pub mod io;
mod led;
mod mapper;
mod profile;
pub mod virtual_flx4;

pub use action::{Action, ControlEvent, GlobalKnob, Knob};
pub use led::{DeckLeds, LedInputs, LedState};
pub use mapper::{Mapper, MidiMessage};
pub use profile::{
    Byte, Encoding, InputBinding, LedSource, OutputBinding, Profile, builtin_profiles,
};
