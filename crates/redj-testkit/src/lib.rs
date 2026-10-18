//! Synthetic, license-free reference tracks with known tempo, downbeats,
//! sections and key.
//!
//! The generator builds a simple four-on-the-floor arrangement (kick, clap,
//! hats, bass, chord pad, crash, build-up riser) whose ground truth is known
//! exactly, so analysis and rendering can be tested deterministically.

use std::f64::consts::TAU;
use std::path::Path;

use redj_core::{Key, Mode, SectionKind};

/// Arrangement and sound parameters of a synthetic track.
#[derive(Debug, Clone)]
pub struct SynthSpec {
    pub bpm: f64,
    pub sample_rate: u32,
    /// Silence before the first downbeat, in seconds.
    pub lead_in_sec: f64,
    pub key: Key,
    /// Sections and their length in bars, in order.
    pub sections: Vec<(SectionKind, u32)>,
    pub seed: u64,
    /// Rise time of the kick (seconds); real kicks range from a click to
    /// tens of milliseconds.
    pub kick_attack_sec: f64,
    /// Gain and decay (seconds) of the off-beat hat; loud open hats can
    /// out-weigh the kick in the full-band onset.
    pub hat_gain: f64,
    pub hat_decay_sec: f64,
}

impl SynthSpec {
    /// A typical club arrangement: 16-bar intro, 8-bar build, 32-bar drop,
    /// 16-bar breakdown, 8-bar build, 32-bar drop, 16-bar outro.
    pub fn club(bpm: f64, key: Key) -> Self {
        use SectionKind::*;
        Self {
            bpm,
            sample_rate: 44_100,
            lead_in_sec: 0.0,
            key,
            sections: vec![
                (Intro, 16),
                (Buildup, 8),
                (Drop, 32),
                (Breakdown, 16),
                (Buildup, 8),
                (Drop, 32),
                (Outro, 16),
            ],
            seed: 1,
            kick_attack_sec: 0.0005,
            hat_gain: 0.12,
            hat_decay_sec: 0.05,
        }
    }

    /// A short arrangement for fast tests (about 40 bars).
    pub fn short(bpm: f64, key: Key) -> Self {
        use SectionKind::*;
        Self {
            sections: vec![(Intro, 8), (Drop, 16), (Breakdown, 8), (Outro, 8)],
            ..Self::club(bpm, key)
        }
    }

    pub fn with_lead_in(mut self, sec: f64) -> Self {
        self.lead_in_sec = sec;
        self
    }

    pub fn with_sample_rate(mut self, sr: u32) -> Self {
        self.sample_rate = sr;
        self
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    pub fn total_bars(&self) -> u32 {
        self.sections.iter().map(|s| s.1).sum()
    }
}

/// Ground truth of a rendered synthetic track.
#[derive(Debug, Clone)]
pub struct Truth {
    pub bpm: f64,
    /// Time of the first downbeat (bar 0).
    pub first_downbeat_sec: f64,
    /// `(kind, start_bar, end_bar)` for each section.
    pub sections: Vec<(SectionKind, u32, u32)>,
    pub key: Key,
    pub total_bars: u32,
}

/// A rendered synthetic track: interleaved stereo `f32` samples.
#[derive(Debug, Clone)]
pub struct SynthTrack {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub truth: Truth,
}

impl SynthTrack {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Writes the track as a 16-bit stereo WAV file.
    pub fn write_wav(&self, path: &Path) -> hound::Result<()> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec)?;
        for &s in &self.samples {
            w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
        w.finalize()
    }
}

struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn midi_to_hz(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

/// Which instruments play in a section.
struct Parts {
    kick: bool,
    clap: bool,
    hats: bool,
    bass: bool,
    pad: f64,
    riser: bool,
}

fn parts(kind: SectionKind) -> Parts {
    use SectionKind::*;
    match kind {
        Intro | Outro => Parts {
            kick: true,
            clap: false,
            hats: true,
            bass: false,
            pad: 0.0,
            riser: false,
        },
        Buildup => Parts {
            kick: false,
            clap: true,
            hats: true,
            bass: false,
            pad: 0.5,
            riser: true,
        },
        Drop => Parts {
            kick: true,
            clap: true,
            hats: true,
            bass: true,
            pad: 0.6,
            riser: false,
        },
        Breakdown => Parts {
            kick: false,
            clap: false,
            hats: false,
            bass: false,
            pad: 1.0,
            riser: false,
        },
        Body => Parts {
            kick: true,
            clap: false,
            hats: true,
            bass: true,
            pad: 0.3,
            riser: false,
        },
    }
}

/// Renders a synthetic track.
pub fn render(spec: &SynthSpec) -> SynthTrack {
    let sr = f64::from(spec.sample_rate);
    let beat = 60.0 / spec.bpm;
    let bar = 4.0 * beat;
    let total_bars = spec.total_bars();
    let tail = 1.0;
    let len_sec = spec.lead_in_sec + f64::from(total_bars) * bar + tail;
    let frames = (len_sec * sr) as usize;
    let mut l = vec![0f64; frames];
    let mut r = vec![0f64; frames];
    let mut rng = Rng(spec.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);

    // Chord progression i - VI - III - VII (minor) or I - V - vi - IV
    // (major), one chord per two bars; offsets from the tonic in semitones.
    let tonic = f64::from(spec.key.tonic);
    let prog: [[f64; 3]; 4] = match spec.key.mode {
        Mode::Minor => [[0., 3., 7.], [8., 12., 15.], [3., 7., 10.], [10., 14., 17.]],
        Mode::Major => [[0., 4., 7.], [7., 11., 14.], [9., 12., 16.], [5., 9., 12.]],
    };

    let at = |t: f64| (t * sr) as usize;
    let mut sections = Vec::new();
    let mut bar_index = 0u32;
    for &(kind, bars) in &spec.sections {
        sections.push((kind, bar_index, bar_index + bars));
        let p = parts(kind);
        for b in bar_index..bar_index + bars {
            let bar_t = spec.lead_in_sec + f64::from(b) * bar;
            let in_section = b - bar_index;
            let chord = prog[((b / 2) % 4) as usize];

            // Crash on the first downbeat of every 8-bar phrase.
            if (in_section % 8 == 0 && kind != SectionKind::Breakdown) || b == bar_index {
                noise_hit(&mut l, &mut r, at(bar_t), 1.6, 0.18, &mut rng, sr, 0.9);
            }
            for q in 0..4 {
                let t = bar_t + f64::from(q) * beat;
                if p.kick {
                    kick(&mut l, &mut r, at(t), sr, 0.9, spec.kick_attack_sec);
                }
                if p.clap && (q == 1 || q == 3) {
                    noise_hit(&mut l, &mut r, at(t), 0.18, 0.25, &mut rng, sr, 0.5);
                }
                if p.hats {
                    noise_hit(
                        &mut l,
                        &mut r,
                        at(t + beat / 2.0),
                        spec.hat_decay_sec,
                        spec.hat_gain,
                        &mut rng,
                        sr,
                        0.95,
                    );
                }
                if p.bass {
                    let root = 36.0 + tonic + chord[0];
                    tone(
                        &mut l,
                        &mut r,
                        at(t + beat / 2.0),
                        beat / 2.0 * 0.9,
                        midi_to_hz(root),
                        sr,
                        0.35,
                        3,
                    );
                }
            }
            if p.riser {
                // Snare roll getting denser through the section.
                let density = 1 + (in_section * 4 / bars.max(1)) as usize;
                for i in 0..(4 * density) {
                    let t = bar_t + i as f64 * bar / (4 * density) as f64;
                    let g = 0.08 + 0.25 * f64::from(in_section) / f64::from(bars);
                    noise_hit(&mut l, &mut r, at(t), 0.08, g, &mut rng, sr, 0.6);
                }
            }
            if p.pad > 0.0 && b % 2 == 0 {
                for &n in &chord {
                    let note = 60.0 + tonic + n;
                    tone(
                        &mut l,
                        &mut r,
                        at(bar_t),
                        2.0 * bar,
                        midi_to_hz(note),
                        sr,
                        0.07 * p.pad,
                        5,
                    );
                }
                // Root an octave below anchors the key in the chroma.
                tone(
                    &mut l,
                    &mut r,
                    at(bar_t),
                    2.0 * bar,
                    midi_to_hz(48.0 + tonic + chord[0]),
                    sr,
                    0.06 * p.pad,
                    3,
                );
            }
        }
        bar_index += bars;
    }

    let peak = l
        .iter()
        .chain(r.iter())
        .fold(0f64, |m, &v| m.max(v.abs()))
        .max(1e-9);
    let norm = 0.89 / peak;
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        samples.push((l[i] * norm) as f32);
        samples.push((r[i] * norm) as f32);
    }
    SynthTrack {
        sample_rate: spec.sample_rate,
        samples,
        truth: Truth {
            bpm: spec.bpm,
            first_downbeat_sec: spec.lead_in_sec,
            sections,
            key: spec.key,
            total_bars,
        },
    }
}

fn kick(l: &mut [f64], r: &mut [f64], start: usize, sr: f64, gain: f64, attack: f64) {
    let n = (0.3 * sr) as usize;
    let mut phase = 0.0;
    for i in 0..n {
        let Some(idx) = start.checked_add(i).filter(|&x| x < l.len()) else {
            break;
        };
        let t = i as f64 / sr;
        let f = 48.0 + 110.0 * (-t * 30.0).exp();
        phase += TAU * f / sr;
        let env = (-t * 9.0).exp() * (1.0 - (-t / attack).exp());
        let v = gain * env * phase.sin();
        l[idx] += v;
        r[idx] += v;
    }
}

#[allow(clippy::too_many_arguments)]
fn noise_hit(
    l: &mut [f64],
    r: &mut [f64],
    start: usize,
    decay: f64,
    gain: f64,
    rng: &mut Rng,
    sr: f64,
    hp: f64,
) {
    let n = (decay * 4.0 * sr) as usize;
    let (mut pl, mut pr) = (0.0, 0.0);
    for i in 0..n {
        let Some(idx) = start.checked_add(i).filter(|&x| x < l.len()) else {
            break;
        };
        let t = i as f64 / sr;
        let env = (-t / decay).exp();
        let (a, b) = (rng.next_f64(), rng.next_f64());
        // One-pole differencing as a cheap high-pass.
        let (vl, vr) = (a - hp * pl, b - hp * pr);
        pl = a;
        pr = b;
        l[idx] += gain * env * vl;
        r[idx] += gain * env * vr;
    }
}

#[allow(clippy::too_many_arguments)]
fn tone(
    l: &mut [f64],
    r: &mut [f64],
    start: usize,
    dur: f64,
    hz: f64,
    sr: f64,
    gain: f64,
    harmonics: u32,
) {
    let n = (dur * sr) as usize;
    let attack = (0.01 * sr).max(1.0);
    let release = (0.05 * sr).max(1.0);
    for i in 0..n {
        let Some(idx) = start.checked_add(i).filter(|&x| x < l.len()) else {
            break;
        };
        let t = i as f64 / sr;
        let fi = i as f64;
        let env = (fi / attack).min(1.0) * ((n as f64 - fi) / release).min(1.0);
        let mut v = 0.0;
        for h in 1..=harmonics {
            let hf = f64::from(h);
            v += (TAU * hz * hf * t).sin() / hf;
        }
        l[idx] += gain * env * v;
        r[idx] += gain * env * v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_expected_length_and_truth() {
        let spec = SynthSpec::short(128.0, Key::new(9, Mode::Minor)).with_lead_in(0.2);
        let t = render(&spec);
        let expected = (0.2 + 40.0 * 4.0 * 60.0 / 128.0 + 1.0) * 44_100.0;
        assert!((t.frames() as f64 - expected).abs() < 2.0);
        assert_eq!(t.truth.total_bars, 40);
        assert_eq!(t.truth.sections[1], (SectionKind::Drop, 8, 24));
        assert!(t.samples.iter().all(|s| s.abs() <= 0.9));
        // Silent lead-in.
        assert!(
            t.samples[..(0.19 * 44_100.0) as usize * 2]
                .iter()
                .all(|&s| s == 0.0)
        );
    }
}
