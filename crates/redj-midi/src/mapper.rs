//! Turns raw MIDI messages into control events, with MIDI learn.

use std::collections::HashMap;

use crate::action::{Action, ControlEvent};
use crate::profile::{Byte, Encoding, InputBinding, Profile};

/// A short MIDI message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiMessage {
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

impl MidiMessage {
    pub fn new(status: u8, data1: u8, data2: u8) -> Self {
        Self {
            status,
            data1,
            data2,
        }
    }

    /// Parses raw bytes (ignores sysex and short real-time messages).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        match b {
            [s, d1, d2, ..] if (0x80..0xF0).contains(s) => Some(Self::new(*s, *d1, *d2)),
            _ => None,
        }
    }

    pub fn to_bytes(self) -> [u8; 3] {
        [self.status, self.data1, self.data2]
    }

    /// Note-off and note-on with velocity 0 are normalized to note-on 0.
    fn normalized(self) -> Self {
        if self.status & 0xF0 == 0x80 {
            Self::new(0x90 | (self.status & 0x0F), self.data1, 0)
        } else {
            self
        }
    }
}

/// Maps MIDI input to [`ControlEvent`]s using a profile plus user
/// overrides from MIDI learn.
#[derive(Debug, Clone)]
pub struct Mapper {
    profile: Option<Profile>,
    /// Learned bindings; they take precedence over the profile.
    overrides: Vec<InputBinding>,
    /// Last MSB per (status, msb controller) for 14-bit controls.
    msb: HashMap<(u8, u8), u8>,
    learning: Option<Action>,
    learned: Option<InputBinding>,
}

impl Mapper {
    pub fn new(profile: Option<Profile>) -> Self {
        Self {
            profile,
            overrides: Vec::new(),
            msb: HashMap::new(),
            learning: None,
            learned: None,
        }
    }

    pub fn profile(&self) -> Option<&Profile> {
        self.profile.as_ref()
    }

    pub fn overrides(&self) -> &[InputBinding] {
        &self.overrides
    }

    pub fn set_overrides(&mut self, o: Vec<InputBinding>) {
        self.overrides = o;
    }

    /// Starts MIDI learn: the next control moved is bound to `action`.
    pub fn learn(&mut self, action: Action) {
        self.learning = Some(action);
    }

    pub fn is_learning(&self) -> bool {
        self.learning.is_some()
    }

    pub fn cancel_learn(&mut self) {
        self.learning = None;
    }

    /// The binding created by the last completed MIDI learn.
    pub fn take_learned(&mut self) -> Option<InputBinding> {
        self.learned.take()
    }

    fn find(&self, m: MidiMessage) -> Option<(&InputBinding, bool)> {
        let all = self
            .overrides
            .iter()
            .chain(self.profile.iter().flat_map(|p| p.inputs.iter()));
        for b in all {
            if b.status.0 != m.status {
                continue;
            }
            if b.data1.0 == m.data1 {
                return Some((b, false));
            }
            if b.lsb.is_some_and(|l| l.0 == m.data1) {
                return Some((b, true));
            }
        }
        None
    }

    /// Handles one message.
    pub fn handle(&mut self, raw: MidiMessage) -> Option<ControlEvent> {
        let m = raw.normalized();
        if let Some(action) = self.learning.take() {
            let kind = m.status & 0xF0;
            let encoding = if action.is_relative() && kind == 0xB0 {
                if m.data2 >= 0x40 - 8 && m.data2 <= 0x40 + 8 {
                    Encoding::Relative64
                } else {
                    Encoding::RelativeTwos
                }
            } else {
                Encoding::Absolute
            };
            let b = InputBinding {
                status: Byte(m.status),
                data1: Byte(m.data1),
                lsb: None,
                invert: false,
                encoding,
                action,
            };
            self.overrides
                .retain(|o| !(o.status == b.status && o.data1 == b.data1));
            self.overrides.insert(0, b.clone());
            self.learned = Some(b);
            return None;
        }
        let (b, is_lsb) = self.find(m)?;
        let b = b.clone();
        let action = b.action;
        if action.is_relative() {
            let ticks = match b.encoding {
                Encoding::Relative64 | Encoding::Absolute => f32::from(m.data2) - 64.0,
                Encoding::RelativeTwos => {
                    if m.data2 < 64 {
                        f32::from(m.data2)
                    } else {
                        f32::from(m.data2) - 128.0
                    }
                }
            };
            return Some(ControlEvent::Delta { action, ticks });
        }
        if action.is_continuous() {
            let mut value = match b.lsb {
                Some(_) if !is_lsb => {
                    // MSB: remember it and wait for the LSB, which follows
                    // immediately (emitting both would make the value jitter).
                    self.msb.insert((m.status, b.data1.0), m.data2);
                    return None;
                }
                Some(_) => {
                    let msb = self.msb.get(&(m.status, b.data1.0)).copied().unwrap_or(0);
                    f32::from((u16::from(msb) << 7) | u16::from(m.data2)) / 16_383.0
                }
                None => f32::from(m.data2) / 127.0,
            };
            if b.invert {
                value = 1.0 - value;
            }
            return Some(ControlEvent::Value { action, value });
        }
        let pressed = m.data2 > 0;
        if !pressed && !action.wants_release() {
            return None;
        }
        Some(ControlEvent::Button { action, pressed })
    }
}
