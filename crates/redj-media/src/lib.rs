//! Audio file decoding (symphonia) and high-quality sample-rate conversion
//! (rubato).
//!
//! Everything works on interleaved `f32` buffers. Decks always play stereo,
//! so [`Audio::into_stereo`] folds mono and multichannel files to two
//! channels.

use std::fs::File;
use std::path::Path;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("cannot open {path}: {source}")]
    Open {
        path: String,
        source: std::io::Error,
    },
    #[error("unsupported or corrupt audio file {path}: {msg}")]
    Format { path: String, msg: String },
    #[error("resampling failed: {0}")]
    Resample(String),
}

/// Decoded audio, interleaved `f32`.
#[derive(Debug, Clone, Default)]
pub struct Audio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Audio {
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / usize::from(self.channels)
        }
    }

    pub fn duration_sec(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Mono mix (average of all channels).
    pub fn to_mono(&self) -> Vec<f32> {
        let ch = usize::from(self.channels.max(1));
        let inv = 1.0 / ch as f32;
        self.samples
            .chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() * inv)
            .collect()
    }

    /// Converts to two channels: mono is duplicated, extra channels beyond
    /// the front pair are folded in at -3 dB.
    pub fn into_stereo(self) -> Audio {
        match self.channels {
            2 => self,
            1 => Audio {
                sample_rate: self.sample_rate,
                channels: 2,
                samples: self.samples.iter().flat_map(|&s| [s, s]).collect(),
            },
            0 => Audio {
                sample_rate: self.sample_rate,
                channels: 2,
                samples: Vec::new(),
            },
            n => {
                let n = usize::from(n);
                let k = std::f32::consts::FRAC_1_SQRT_2;
                let samples = self
                    .samples
                    .chunks_exact(n)
                    .flat_map(|f| {
                        let rest: f32 = f[2..].iter().sum::<f32>() * k;
                        [f[0] + rest, f[1] + rest]
                    })
                    .collect();
                Audio {
                    sample_rate: self.sample_rate,
                    channels: 2,
                    samples,
                }
            }
        }
    }

    /// Resamples to `rate` with rubato's FFT resampler (no-op if equal).
    pub fn resampled(self, rate: u32) -> Result<Audio, MediaError> {
        if rate == self.sample_rate || self.samples.is_empty() {
            return Ok(Audio {
                sample_rate: rate,
                ..self
            });
        }
        let ch = usize::from(self.channels);
        let frames = self.frames();
        let mut rs = Fft::<f32>::new(
            self.sample_rate as usize,
            rate as usize,
            4096,
            ch,
            FixedSync::Both,
        )
        .map_err(|e| MediaError::Resample(e.to_string()))?;
        let input = InterleavedSlice::new(&self.samples[..], ch, frames)
            .map_err(|e| MediaError::Resample(e.to_string()))?;
        let out = rs
            .process_all(&input, frames, None)
            .map_err(|e| MediaError::Resample(e.to_string()))?;
        Ok(Audio {
            sample_rate: rate,
            channels: self.channels,
            samples: out.take_data(),
        })
    }
}

/// Decodes a whole file into memory.
pub fn decode_file(path: &Path) -> Result<Audio, MediaError> {
    let mut audio = Audio::default();
    decode_file_with(path, |rate, channels, chunk| {
        audio.sample_rate = rate;
        audio.channels = channels;
        audio.samples.extend_from_slice(chunk);
        true
    })?;
    Ok(audio)
}

/// Decodes a file, handing each decoded chunk (interleaved) to `sink`
/// together with the sample rate and channel count. `sink` returns `false`
/// to stop early. Lets a loader publish audio while decoding progresses.
pub fn decode_file_with(
    path: &Path,
    mut sink: impl FnMut(u32, u16, &[f32]) -> bool,
) -> Result<(), MediaError> {
    let p = path.display().to_string();
    let file = File::open(path).map_err(|source| MediaError::Open {
        path: p.clone(),
        source,
    })?;
    let fmt_err = |e: SymError| MediaError::Format {
        path: p.clone(),
        msg: e.to_string(),
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(fmt_err)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| MediaError::Format {
            path: p.clone(),
            msg: "no audio track".into(),
        })?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|c| c.audio())
        .ok_or_else(|| MediaError::Format {
            path: p.clone(),
            msg: "no audio codec parameters".into(),
        })?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(fmt_err)?;
    let track_id = track.id;
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(fmt_err(e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = decoded.spec();
                let rate = spec.rate();
                let channels = spec.channels().count() as u16;
                buf.resize(decoded.samples_interleaved(), 0.0);
                decoded.copy_to_slice_interleaved(&mut buf);
                if !sink(rate, channels, &buf) {
                    break;
                }
            }
            // Corrupt packets are skipped, as players do.
            Err(SymError::DecodeError(_)) => {}
            Err(e) => return Err(fmt_err(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use redj_core::{Key, Mode};
    use redj_testkit::{SynthSpec, render};

    #[test]
    fn decodes_wav_written_by_testkit() {
        let dir = std::env::temp_dir().join(format!("redj-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let t = render(&SynthSpec::short(124.0, Key::new(0, Mode::Major)));
        t.write_wav(&path).unwrap();
        let a = decode_file(&path).unwrap();
        assert_eq!(a.sample_rate, 44_100);
        assert_eq!(a.channels, 2);
        assert_eq!(a.frames(), t.frames());
        let err: f32 = a
            .samples
            .iter()
            .zip(&t.samples)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-4, "max error {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resampling_preserves_duration_and_tone() {
        let sr = 44_100u32;
        let n = sr as usize * 2;
        let samples: Vec<f32> = (0..n)
            .flat_map(|i| {
                let v =
                    (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(sr)).sin() as f32 * 0.5;
                [v, v]
            })
            .collect();
        let a = Audio {
            sample_rate: sr,
            channels: 2,
            samples,
        };
        let b = a.resampled(48_000).unwrap();
        assert_eq!(b.sample_rate, 48_000);
        assert!((b.frames() as i64 - 96_000).abs() <= 2, "{}", b.frames());
        // Compare against an ideal 1 kHz sine in the steady-state middle.
        let mid = 48_000;
        let mut err = 0f32;
        for i in mid..mid + 1000 {
            let ideal = (std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0).sin() as f32 * 0.5;
            err = err.max((b.samples[i * 2] - ideal).abs());
        }
        assert!(err < 1e-3, "error {err}");
    }

    #[test]
    fn stereo_folding() {
        let mono = Audio {
            sample_rate: 1,
            channels: 1,
            samples: vec![0.1, 0.2],
        };
        assert_eq!(mono.into_stereo().samples, vec![0.1, 0.1, 0.2, 0.2]);
    }
}
