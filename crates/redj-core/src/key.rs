use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Major,
    Minor,
}

/// Musical key. Serialized with its Camelot code (`"8A"`) and a readable
/// name so a language model can use either.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "KeyRepr", into = "KeyRepr")]
pub struct Key {
    /// Pitch class of the tonic, 0 = C .. 11 = B.
    pub tonic: u8,
    pub mode: Mode,
    /// Detector confidence in 0..1 (correlation margin); key detection is
    /// approximate and this tells how much to trust it.
    pub confidence: f32,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct KeyRepr {
    /// Camelot wheel code, e.g. "8A" (A minor) or "8B" (C major).
    camelot: String,
    /// Readable name, e.g. "A minor".
    #[serde(default)]
    name: String,
    #[serde(default)]
    confidence: f32,
}

impl From<Key> for KeyRepr {
    fn from(k: Key) -> Self {
        KeyRepr {
            camelot: k.camelot(),
            name: k.to_string(),
            confidence: k.confidence,
        }
    }
}

impl TryFrom<KeyRepr> for Key {
    type Error = String;
    fn try_from(r: KeyRepr) -> Result<Self, Self::Error> {
        let mut k = Key::from_camelot(&r.camelot)
            .ok_or_else(|| format!("invalid Camelot key code {:?}", r.camelot))?;
        k.confidence = r.confidence;
        Ok(k)
    }
}

impl Key {
    pub fn new(tonic: u8, mode: Mode) -> Self {
        Self {
            tonic: tonic % 12,
            mode,
            confidence: 1.0,
        }
    }

    /// Camelot number 1..=12.
    pub fn camelot_number(&self) -> u8 {
        let major_tonic = match self.mode {
            Mode::Major => self.tonic,
            Mode::Minor => (self.tonic + 3) % 12,
        };
        ((u32::from(major_tonic) * 7 + 7) % 12) as u8 + 1
    }

    /// Camelot code, e.g. `"8A"`.
    pub fn camelot(&self) -> String {
        let letter = match self.mode {
            Mode::Minor => 'A',
            Mode::Major => 'B',
        };
        format!("{}{}", self.camelot_number(), letter)
    }

    /// Parses a Camelot code (`"8A"`, `"12b"`).
    pub fn from_camelot(code: &str) -> Option<Key> {
        let code = code.trim();
        let (num, letter) = code.split_at(code.len().checked_sub(1)?);
        let n: u32 = num.parse().ok()?;
        if !(1..=12).contains(&n) {
            return None;
        }
        let mode = match letter {
            "A" | "a" => Mode::Minor,
            "B" | "b" => Mode::Major,
            _ => return None,
        };
        // Invert number = (major_tonic * 7 + 7) % 12 + 1; 7 is its own
        // inverse modulo 12.
        let major_tonic = (((n + 12 - 1 - 7) % 12) * 7 % 12) as u8;
        let tonic = match mode {
            Mode::Major => major_tonic,
            Mode::Minor => (major_tonic + 9) % 12,
        };
        Some(Key::new(tonic, mode))
    }

    /// Harmonic distance on the Camelot wheel: 0 = same key, 1 = adjacent
    /// (perfect fifth or relative major/minor), larger = less compatible.
    pub fn camelot_distance(&self, other: &Key) -> u8 {
        let a = i32::from(self.camelot_number());
        let b = i32::from(other.camelot_number());
        let d = (a - b).rem_euclid(12);
        let steps = d.min(12 - d) as u8;
        steps + u8::from(self.mode != other.mode)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = match self.mode {
            Mode::Major => "major",
            Mode::Minor => "minor",
        };
        write!(f, "{} {}", NAMES[usize::from(self.tonic % 12)], mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_codes() {
        assert_eq!(Key::new(9, Mode::Minor).camelot(), "8A");
        assert_eq!(Key::new(0, Mode::Major).camelot(), "8B");
        assert_eq!(Key::new(11, Mode::Major).camelot(), "1B");
        assert_eq!(Key::new(8, Mode::Minor).camelot(), "1A");
        assert_eq!(Key::new(1, Mode::Minor).camelot(), "12A");
    }

    #[test]
    fn camelot_round_trip() {
        for t in 0..12 {
            for m in [Mode::Major, Mode::Minor] {
                let k = Key::new(t, m);
                let back = Key::from_camelot(&k.camelot()).unwrap();
                assert_eq!((back.tonic, back.mode), (t, m), "{}", k.camelot());
            }
        }
    }

    #[test]
    fn distances() {
        let am = Key::new(9, Mode::Minor);
        assert_eq!(am.camelot_distance(&Key::new(0, Mode::Major)), 1);
        assert_eq!(am.camelot_distance(&Key::new(4, Mode::Minor)), 1);
        assert_eq!(am.camelot_distance(&am), 0);
        assert_eq!(am.camelot_distance(&Key::new(3, Mode::Minor)), 6);
    }

    #[test]
    fn serde_uses_camelot() {
        let k = Key::new(9, Mode::Minor);
        let s = serde_json::to_string(&k).unwrap();
        assert!(s.contains("\"8A\"") && s.contains("A minor"));
        let back: Key = serde_json::from_str(&s).unwrap();
        assert_eq!(back.tonic, 9);
    }
}
