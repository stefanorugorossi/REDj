use std::sync::Arc;

use redj_dsp::{SincTable, Source, TimeStretcher, Wsola};

/// Reads a deck's source at the position the playhead asks for, either
/// varispeed (sinc interpolation, pitch follows speed) or keylock
/// (WSOLA). Discontinuities in varispeed mode (loops, cue jumps) are
/// smoothed with a short cross-fade between the old and the new read head.
#[derive(Debug, Clone)]
pub struct DeckReader {
    sinc: Arc<SincTable>,
    wsola: Wsola,
    keylock: bool,
    last_pos: f64,
    last_rate: f64,
    has_history: bool,
    /// Old read head and remaining cross-fade frames after a jump.
    fade_pos: f64,
    fade_left: u32,
    fade_len: u32,
}

impl DeckReader {
    pub fn new(sample_rate: u32, sinc: Arc<SincTable>) -> Self {
        Self {
            sinc,
            wsola: Wsola::new(sample_rate as f32),
            keylock: false,
            last_pos: 0.0,
            last_rate: 1.0,
            has_history: false,
            fade_pos: 0.0,
            fade_left: 0,
            fade_len: (sample_rate as f32 * 0.008) as u32,
        }
    }

    pub fn set_keylock(&mut self, on: bool) {
        if on != self.keylock {
            self.keylock = on;
            self.reset();
        }
    }

    pub fn keylock(&self) -> bool {
        self.keylock
    }

    pub fn reset(&mut self) {
        self.wsola.reset();
        self.has_history = false;
        self.fade_left = 0;
    }

    /// Reads the frame at source position `pos` (frames).
    #[inline]
    pub fn read<S: Source>(&mut self, src: &S, pos: f64) -> (f32, f32) {
        if self.keylock {
            return self.wsola.next_frame(src, pos);
        }
        if self.has_history {
            let expected = self.last_pos + self.last_rate;
            if (pos - expected).abs() > 2.0 {
                // Jump: keep the old head running for a short cross-fade.
                self.fade_pos = expected;
                self.fade_left = self.fade_len;
            } else {
                self.last_rate = pos - self.last_pos;
            }
        }
        self.last_pos = pos;
        self.has_history = true;
        let (mut l, mut r) = self.sinc.read_stereo(src, pos);
        if self.fade_left > 0 {
            let g = self.fade_left as f32 / self.fade_len as f32;
            let (ol, or) = self.sinc.read_stereo(src, self.fade_pos);
            l = l * (1.0 - g) + ol * g;
            r = r * (1.0 - g) + or * g;
            self.fade_pos += self.last_rate;
            self.fade_left -= 1;
        }
        (l, r)
    }
}
