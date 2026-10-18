//! Track analysis: tempo, beat grid, downbeats, sections, energy, loudness,
//! key; JSON cache.
//!
//! Pure Rust and classical DSP: the target material is 4/4
//! four-on-the-floor music, where onset autocorrelation plus a whole-track
//! constant-grid fit is very accurate. Variable-tempo tracks fall back to a
//! dynamic-programming beat tracker and get a grid with anchors.

mod beats;
mod cache;
mod downbeat;
mod features;
mod key;
mod sections;
mod tempo;

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use redj_core::{BeatAnchor, BeatGrid, Loudness, Section, TrackId, TrackInfo, TrackTags};
use redj_media::{Audio, MediaError};

pub use cache::Cache;
pub use features::ANALYSIS_RATE;

/// Bumped whenever analysis results change, invalidating cached records.
pub const ANALYZER_VERSION: u32 = 4;

/// File extensions picked up by [`scan_dir`].
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "flac", "aiff", "aif", "ogg", "m4a"];

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error("i/o error on {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// Analysis settings.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisOptions {
    /// Tempo range; detected tempos are folded into `[min_bpm, max_bpm)`.
    pub min_bpm: f64,
    pub max_bpm: f64,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            min_bpm: 86.0,
            max_bpm: 176.0,
        }
    }
}

/// Musical analysis of decoded audio (everything but file metadata).
#[derive(Debug, Clone)]
pub struct AudioAnalysis {
    pub grid: BeatGrid,
    pub sections: Vec<Section>,
    pub bar_energy: Vec<f32>,
    pub loudness: Loudness,
    pub key: Option<redj_core::Key>,
}

/// Analyzes decoded audio.
pub fn analyze_audio(
    audio: &Audio,
    opts: &AnalysisOptions,
) -> Result<AudioAnalysis, AnalysisError> {
    let duration = audio.duration_sec();
    let mono = Audio {
        sample_rate: audio.sample_rate,
        channels: 1,
        samples: audio.to_mono(),
    }
    .resampled(ANALYSIS_RATE)?
    .samples;
    let feats = features::compute(&mono);
    let env = features::detrend(&feats.onset, (feats.fps * 0.5) as usize);
    let grid = beat_grid(&feats, &env, &mono, duration, opts);
    let bf = sections::bar_features(&feats, &grid, duration);
    let sections = sections::segment(&bf);
    let key = key::detect(&key::chroma(&mono));
    Ok(AudioAnalysis {
        grid,
        sections,
        bar_energy: bf.energy,
        loudness: loudness(audio),
        key,
    })
}

fn beat_grid(
    feats: &features::Features,
    env: &[f32],
    mono: &[f32],
    duration: f64,
    opts: &AnalysisOptions,
) -> BeatGrid {
    let fps = feats.fps;
    let Some(bpm0) = tempo::coarse_bpm(env, fps, opts.min_bpm, opts.max_bpm) else {
        return BeatGrid::constant(120.0, 0.0);
    };
    let kick_env = features::detrend(&feats.low_onset, (fps * 0.5) as usize);
    let bpm0 = tempo::resolve_octave(&kick_env, fps, bpm0, opts.min_bpm, opts.max_bpm);
    let (bpm, env_phase) = tempo::refine(env, fps, bpm0);
    let period = 60.0 * fps / bpm;
    let (kick_sharp, kick_phase) = tempo::fold(&kick_env, period, 64);
    let phase_frames = if kick_sharp >= 1.5 {
        kick_phase
    } else {
        env_phase
    };
    let to_sec = |frame: f64| {
        (frame * features::HOP as f64 + (features::N_FFT / 2) as f64) / f64::from(ANALYSIS_RATE)
    };

    if beats::grid_misfit(&kick_env, period) > 0.25 {
        let frames = beats::track(env, period);
        if frames.len() >= 16 {
            let times: Vec<f64> = frames.iter().map(|&f| to_sec(f)).collect();
            let offset = onset_offset(mono, &times);
            let times: Vec<f64> = times.iter().map(|t| t + offset).collect();
            let phase = downbeat::detect(feats, &times, env);
            let avg_bpm = 60.0 * (times.len() - 1) as f64 / (times[times.len() - 1] - times[0]);
            let anchors = times
                .iter()
                .enumerate()
                .map(|(i, &sec)| BeatAnchor {
                    beat: i as f64 - phase as f64,
                    sec,
                })
                .collect();
            return BeatGrid {
                bpm: avg_bpm,
                origin_sec: times[phase],
                anchors,
            };
        }
    }

    let beat_len = 60.0 / bpm;
    let mut t0 = to_sec(phase_frames);
    let mut beats_t: Vec<f64> = Vec::new();
    let mut t = t0.rem_euclid(beat_len);
    while t < duration {
        beats_t.push(t);
        t += beat_len;
    }
    let off = onset_offset(mono, &beats_t);
    t0 = t0.rem_euclid(beat_len) + off;
    // Re-anchor the beat list after the correction (first beat at >= 0).
    let first = t0.rem_euclid(beat_len);
    let beats_t: Vec<f64> = (0..)
        .map(|k| first + k as f64 * beat_len)
        .take_while(|&t| t < duration)
        .collect();
    let phase = downbeat::detect(feats, &beats_t, env);
    BeatGrid::constant(bpm, first + phase as f64 * beat_len)
}

/// Fine-aligns predicted beat times to kick transients in the time domain.
/// Averages the low-passed envelope around every predicted beat and returns
/// the offset (seconds) of the start of the rise.
///
/// The window reaches further back than forward: the onset envelope that
/// placed the beats peaks after the attack, by up to ~50 ms on kicks with a
/// soft attack.
fn onset_offset(mono: &[f32], beats: &[f64]) -> f64 {
    let sr = f64::from(ANALYSIS_RATE);
    // Two cascaded one-pole low-passes at ~150 Hz, rectified, then a fast
    // envelope follower.
    let a = (-std::f64::consts::TAU * 150.0 / sr).exp();
    let ae = (-1.0 / (0.002 * sr)).exp();
    let (mut y1, mut y2, mut e) = (0f64, 0f64, 0f64);
    let envelope: Vec<f32> = mono
        .iter()
        .map(|&x| {
            y1 = a * y1 + (1.0 - a) * f64::from(x);
            y2 = a * y2 + (1.0 - a) * y1;
            e = ae * e + (1.0 - ae) * y2.abs();
            e as f32
        })
        .collect();
    let before = (0.080 * sr) as i64;
    let after = (0.040 * sr) as i64;
    let mut avg = vec![0f64; (before + after + 1) as usize];
    let mut count = 0;
    for &t in beats {
        let c = (t * sr).round() as i64;
        if c - before < 0 || c + after >= envelope.len() as i64 {
            continue;
        }
        for (k, v) in avg.iter_mut().enumerate() {
            *v += f64::from(envelope[(c - before + k as i64) as usize]);
        }
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    // The valley before the kick (previous tail, sidechain dip), then the
    // peak after it; the attack starts where the envelope last sits within
    // 10% of that rise above the valley.
    let argmax = |r: std::ops::Range<usize>, sign: f64| {
        r.max_by(|&i, &j| (sign * avg[i]).total_cmp(&(sign * avg[j])))
            .expect("non-empty window")
    };
    let lo = argmax(0..(before + (0.010 * sr) as i64) as usize, -1.0);
    let hi = argmax(lo..avg.len(), 1.0);
    let thr = avg[lo] + 0.1 * (avg[hi] - avg[lo]);
    if avg[hi] - avg[lo] <= 0.0 {
        return 0.0;
    }
    let k = (lo..=hi).rev().find(|&k| avg[k] <= thr).unwrap_or(lo);
    (k as i64 - before) as f64 / sr
}

fn loudness(audio: &Audio) -> Loudness {
    use ebur128::{EbuR128, Mode};
    let ch = u32::from(audio.channels.max(1));
    let mut m = match EbuR128::new(ch, audio.sample_rate, Mode::I | Mode::TRUE_PEAK | Mode::LRA) {
        Ok(m) => m,
        Err(_) => {
            return Loudness {
                integrated_lufs: -70.0,
                true_peak_dbtp: -70.0,
                range_lu: 0.0,
            };
        }
    };
    let _ = m.add_frames_f32(&audio.samples);
    let peak = (0..ch)
        .filter_map(|c| m.true_peak(c).ok())
        .fold(0f64, f64::max);
    Loudness {
        integrated_lufs: m.loudness_global().unwrap_or(-70.0).max(-70.0),
        true_peak_dbtp: (20.0 * peak.max(1e-9).log10()).max(-70.0),
        range_lu: m.loudness_range().unwrap_or(0.0),
    }
}

/// Reads tag metadata; missing or unreadable tags yield empty fields.
pub fn read_tags(path: &Path) -> TrackTags {
    use lofty::prelude::*;
    let Ok(file) = lofty::read_from_path(path) else {
        return TrackTags::default();
    };
    let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) else {
        return TrackTags::default();
    };
    let s = |v: Option<std::borrow::Cow<'_, str>>| {
        v.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty())
    };
    let bpm = tag
        .get_string(ItemKey::Bpm)
        .or_else(|| tag.get_string(ItemKey::IntegerBpm))
        .and_then(|b| b.trim().parse::<f64>().ok());
    TrackTags {
        title: s(tag.title()),
        artist: s(tag.artist()),
        album: s(tag.album()),
        genre: s(tag.genre()),
        bpm,
    }
}

/// BLAKE3 hash of a file's content, hex encoded.
pub fn content_hash(path: &Path) -> Result<String, AnalysisError> {
    let io = |source| AnalysisError::Io {
        path: path.display().to_string(),
        source,
    };
    let mut hasher = blake3::Hasher::new();
    let file = std::fs::File::open(path).map_err(io)?;
    hasher.update_reader(file).map_err(io)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Track id derived from a content hash.
pub fn track_id(hash: &str) -> TrackId {
    TrackId(hash[..16.min(hash.len())].to_owned())
}

/// Decodes and analyzes one file (no cache).
pub fn analyze_file(path: &Path, opts: &AnalysisOptions) -> Result<TrackInfo, AnalysisError> {
    let hash = content_hash(path)?;
    analyze_file_with_hash(path, &hash, opts)
}

fn analyze_file_with_hash(
    path: &Path,
    hash: &str,
    opts: &AnalysisOptions,
) -> Result<TrackInfo, AnalysisError> {
    let audio = redj_media::decode_file(path)?;
    let a = analyze_audio(&audio, opts)?;
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(TrackInfo {
        id: track_id(hash),
        tags: read_tags(&path),
        path,
        duration_sec: audio.duration_sec(),
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        grid: a.grid,
        sections: a.sections,
        bar_energy: a.bar_energy,
        loudness: a.loudness,
        key: a.key,
        analyzer_version: ANALYZER_VERSION,
    })
}

/// Analyzes one file, using and filling the cache.
pub fn analyze_cached(
    path: &Path,
    cache: Option<&Cache>,
    opts: &AnalysisOptions,
) -> Result<TrackInfo, AnalysisError> {
    let hash = content_hash(path)?;
    if let Some(c) = cache
        && let Some(mut info) = c.load(&hash)
    {
        // The file may have moved since it was cached.
        info.path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        return Ok(info);
    }
    let info = analyze_file_with_hash(path, &hash, opts)?;
    if let Some(c) = cache
        && let Err(e) = c.store(&hash, &info)
    {
        tracing::warn!("cannot write analysis cache: {e}");
    }
    Ok(info)
}

/// Analyzes many files in parallel. Results keep the input order.
pub fn analyze_many(
    paths: &[PathBuf],
    cache: Option<&Cache>,
    opts: &AnalysisOptions,
) -> Vec<Result<TrackInfo, AnalysisError>> {
    paths
        .par_iter()
        .map(|p| analyze_cached(p, cache, opts))
        .collect()
}

/// Lists audio files in a directory (non-recursive), sorted by name.
pub fn scan_dir(dir: &Path) -> Result<Vec<PathBuf>, AnalysisError> {
    let io = |source| AnalysisError::Io {
        path: dir.display().to_string(),
        source,
    };
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Default analysis cache in the user's cache directory.
pub fn default_cache() -> Option<Cache> {
    directories::ProjectDirs::from("org", "REDj", "redj")
        .map(|d| Cache::new(d.cache_dir().join("analysis")))
}

/// Scans directories (non-recursive) and analyzes every audio file found.
/// Returns the library and the files that failed.
pub fn library_from_dirs(
    dirs: &[PathBuf],
    cache: Option<&Cache>,
    opts: &AnalysisOptions,
) -> (redj_core::Library, Vec<(PathBuf, AnalysisError)>) {
    let mut paths = Vec::new();
    let mut failed = Vec::new();
    for d in dirs {
        if d.is_file() {
            paths.push(d.clone());
            continue;
        }
        match scan_dir(d) {
            Ok(p) => paths.extend(p),
            Err(e) => failed.push((d.clone(), e)),
        }
    }
    let mut lib = redj_core::Library::new();
    for (p, r) in paths.iter().zip(analyze_many(&paths, cache, opts)) {
        match r {
            Ok(info) => lib.insert(info),
            Err(e) => failed.push((p.clone(), e)),
        }
    }
    (lib, failed)
}
