//! A virtual DDJ-FLX4: the messages the hardware sends for each gesture,
//! for tests and for trying the console without the controller.

use crate::mapper::MidiMessage;

fn note(deck: usize) -> u8 {
    0x90 | deck as u8
}

fn cc(deck: usize) -> u8 {
    0xB0 | deck as u8
}

/// Press and release of a deck button (`data1` from the message list).
pub fn button(deck: usize, data1: u8) -> [MidiMessage; 2] {
    [
        MidiMessage::new(note(deck), data1, 0x7F),
        MidiMessage::new(note(deck), data1, 0x00),
    ]
}

pub fn play(deck: usize) -> [MidiMessage; 2] {
    button(deck, 0x0B)
}

pub fn cue_press(deck: usize) -> MidiMessage {
    MidiMessage::new(note(deck), 0x0C, 0x7F)
}

pub fn cue_release(deck: usize) -> MidiMessage {
    MidiMessage::new(note(deck), 0x0C, 0x00)
}

pub fn sync(deck: usize) -> [MidiMessage; 2] {
    button(deck, 0x58)
}

/// A pad in hot cue mode (index 0..8).
pub fn hot_cue_pad(deck: usize, index: u8) -> [MidiMessage; 2] {
    let s = 0x97 + 2 * deck as u8;
    [
        MidiMessage::new(s, index, 0x7F),
        MidiMessage::new(s, index, 0),
    ]
}

/// A 14-bit control: MSB on `msb`, LSB on `msb + 0x20`, value 0..=16383.
pub fn control14(status: u8, msb: u8, value: u16) -> [MidiMessage; 2] {
    let v = value.min(16_383);
    [
        MidiMessage::new(status, msb, (v >> 7) as u8),
        MidiMessage::new(status, msb + 0x20, (v & 0x7F) as u8),
    ]
}

pub fn channel_fader(deck: usize, value: u16) -> [MidiMessage; 2] {
    control14(cc(deck), 0x13, value)
}

pub fn eq_low(deck: usize, value: u16) -> [MidiMessage; 2] {
    control14(cc(deck), 0x0F, value)
}

pub fn tempo(deck: usize, value: u16) -> [MidiMessage; 2] {
    control14(cc(deck), 0x00, value)
}

pub fn crossfader(value: u16) -> [MidiMessage; 2] {
    control14(0xB6, 0x1F, value)
}

/// Touch (or release) the jog platter.
pub fn jog_touch(deck: usize, on: bool) -> MidiMessage {
    MidiMessage::new(note(deck), 0x36, if on { 0x7F } else { 0 })
}

/// Turn the jog platter by `ticks` (one message per tick, as the hardware
/// sends at slow speeds).
pub fn jog_turn(deck: usize, ticks: i32) -> Vec<MidiMessage> {
    let v = if ticks >= 0 { 65 } else { 63 };
    (0..ticks.unsigned_abs())
        .map(|_| MidiMessage::new(cc(deck), 0x22, v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Action, ControlEvent, DeckLeds, GlobalKnob, Knob, LedInputs, LedState, Mapper,
        builtin_profiles,
    };

    fn mapper() -> Mapper {
        Mapper::new(builtin_profiles().into_iter().next())
    }

    #[test]
    fn profile_parses_and_matches_port() {
        let p = &builtin_profiles()[0];
        assert!(p.matches_port("DDJ-FLX4 MIDI 1"));
        assert!(!p.matches_port("Some Keyboard"));
        assert!(p.inputs.len() > 100);
    }

    #[test]
    fn buttons_map_to_actions() {
        let mut m = mapper();
        let [p, r] = play(1);
        assert_eq!(
            m.handle(p),
            Some(ControlEvent::Button {
                action: Action::PlayToggle { deck: 1 },
                pressed: true
            })
        );
        // Releases of plain buttons are ignored, CUE releases are not.
        assert_eq!(m.handle(r), None);
        assert!(matches!(
            m.handle(cue_release(0)),
            Some(ControlEvent::Button {
                action: Action::Cue { deck: 0 },
                pressed: false
            })
        ));
        // Note-off is a release too.
        assert!(matches!(
            m.handle(MidiMessage::new(0x80, 0x0C, 0x40)),
            Some(ControlEvent::Button { pressed: false, .. })
        ));
        let [h, _] = hot_cue_pad(0, 3);
        assert_eq!(
            m.handle(h),
            Some(ControlEvent::Button {
                action: Action::HotCue { deck: 0, index: 3 },
                pressed: true
            })
        );
    }

    #[test]
    fn fourteen_bit_controls() {
        let mut m = mapper();
        let [msb, lsb] = channel_fader(0, 8192);
        assert_eq!(m.handle(msb), None, "waits for the LSB");
        match m.handle(lsb) {
            Some(ControlEvent::Value {
                action:
                    Action::Knob {
                        deck: 0,
                        knob: Knob::Volume,
                    },
                value,
            }) => assert!((value - 8192.0 / 16383.0).abs() < 1e-6),
            e => panic!("{e:?}"),
        }
        // The tempo fader is inverted.
        let [a, b] = tempo(1, 0);
        m.handle(a);
        assert!(matches!(m.handle(b), Some(ControlEvent::Value { value, .. }) if value == 1.0));
        let [a, b] = crossfader(16_383);
        m.handle(a);
        assert!(matches!(
            m.handle(b),
            Some(ControlEvent::Value { action: Action::Global { knob: GlobalKnob::Crossfader }, value }) if value == 1.0
        ));
    }

    #[test]
    fn jog_is_relative() {
        let mut m = mapper();
        let ev: Vec<_> = jog_turn(0, -3)
            .into_iter()
            .filter_map(|x| m.handle(x))
            .collect();
        assert_eq!(ev.len(), 3);
        assert!(
            ev.iter()
                .all(|e| matches!(e, ControlEvent::Delta { ticks, .. } if *ticks == -1.0))
        );
        assert!(matches!(
            m.handle(jog_touch(0, true)),
            Some(ControlEvent::Button {
                action: Action::JogTouch { deck: 0 },
                pressed: true
            })
        ));
    }

    #[test]
    fn midi_learn_overrides_profile() {
        let mut m = mapper();
        m.learn(Action::AiToggle);
        let msg = MidiMessage::new(0x9F, 0x10, 0x7F);
        assert_eq!(m.handle(msg), None);
        let b = m.take_learned().unwrap();
        assert_eq!(b.action, Action::AiToggle);
        assert_eq!(
            m.handle(msg),
            Some(ControlEvent::Button {
                action: Action::AiToggle,
                pressed: true
            })
        );
        // Re-learning an existing control replaces the profile mapping.
        m.learn(Action::Pfl { deck: 1 });
        let [p, _] = play(0);
        m.handle(p);
        assert_eq!(
            m.handle(p),
            Some(ControlEvent::Button {
                action: Action::Pfl { deck: 1 },
                pressed: true
            })
        );
    }

    #[test]
    fn leds_send_only_changes() {
        let p = &builtin_profiles()[0];
        let mut leds = LedState::default();
        let mut s = LedInputs {
            decks: vec![DeckLeds::default(); 2],
            ai_on: false,
        };
        let first = leds.update(p, &s);
        assert_eq!(first.len(), p.outputs.len());
        assert!(leds.update(p, &s).is_empty());
        s.decks[0].playing = true;
        s.decks[1].hot_cues[2] = true;
        s.decks[0].vu = 0.5;
        let msgs = leds.update(p, &s);
        assert!(msgs.contains(&MidiMessage::new(0x90, 0x0B, 0x7F)));
        assert!(msgs.contains(&MidiMessage::new(0x99, 0x02, 0x7F)));
        assert!(msgs.contains(&MidiMessage::new(0xB0, 0x02, 64)));
        assert_eq!(msgs.len(), 3);
    }
}
