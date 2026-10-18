//! Data-driven controller profiles.

use serde::{Deserialize, Deserializer, Serialize};

use crate::action::Action;

/// A byte written as a number or as a `"0x.."` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Byte(pub u8);

impl<'de> Deserialize<'de> for Byte {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u8),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(Byte(n)),
            Raw::S(s) => {
                let t = s.trim();
                let v = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    Some(h) => u8::from_str_radix(h, 16),
                    None => t.parse(),
                };
                v.map(Byte)
                    .map_err(|_| serde::de::Error::custom(format!("invalid byte {s:?}")))
            }
        }
    }
}

/// How a relative control encodes movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// Absolute value (knobs, faders, buttons).
    #[default]
    Absolute,
    /// Relative around 64 (64 = no movement, 65 = +1, 63 = -1).
    Relative64,
    /// Relative two's complement (1 = +1, 127 = -1).
    RelativeTwos,
}

/// One input mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputBinding {
    /// Status byte (note on 0x9n or control change 0xBn).
    pub status: Byte,
    pub data1: Byte,
    /// LSB controller number for 14-bit controls (`data1` is the MSB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsb: Option<Byte>,
    /// Invert the value (faders mounted upside down).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    #[serde(default)]
    pub encoding: Encoding,
    pub action: Action,
}

/// State that drives an LED.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LedSource {
    Playing {
        deck: usize,
    },
    /// On while paused at the cue point.
    CueIndicator {
        deck: usize,
    },
    Pfl {
        deck: usize,
    },
    Sync {
        deck: usize,
    },
    HotCueSet {
        deck: usize,
        index: usize,
    },
    LoopActive {
        deck: usize,
    },
    BeatLoop {
        deck: usize,
        beats: f64,
    },
    Loaded {
        deck: usize,
    },
    /// VU meter: the value is the level scaled to 0..`on`.
    Vu {
        deck: usize,
    },
    /// Lit while the AI is on.
    Ai,
}

/// One output (LED) mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputBinding {
    pub status: Byte,
    pub data1: Byte,
    pub source: LedSource,
    #[serde(default = "default_on")]
    pub on: u8,
    #[serde(default)]
    pub off: u8,
}

fn default_on() -> u8 {
    0x7F
}

/// A controller profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Substrings of MIDI port names this profile applies to.
    pub port_match: Vec<String>,
    pub decks: usize,
    /// Where the mapping data comes from (credit).
    #[serde(default)]
    pub source: String,
    pub inputs: Vec<InputBinding>,
    #[serde(default)]
    pub outputs: Vec<OutputBinding>,
    /// Messages sent when the controller is connected.
    #[serde(default)]
    pub init: Vec<[Byte; 3]>,
}

impl Profile {
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Whether a port name belongs to this controller.
    pub fn matches_port(&self, port: &str) -> bool {
        let p = port.to_ascii_lowercase();
        self.port_match
            .iter()
            .any(|m| p.contains(&m.to_ascii_lowercase()))
    }
}

/// Built-in profiles.
pub fn builtin_profiles() -> Vec<Profile> {
    [
        include_str!("../profiles/pioneer-ddj-flx4.json"),
        include_str!("../profiles/pioneer-ddj-400.json"),
        include_str!("../profiles/hercules-inpulse-200.json"),
        include_str!("../profiles/hercules-inpulse-300.json"),
    ]
    .iter()
    .map(|s| Profile::from_json(s).expect("built-in profile parses"))
    .collect()
}
