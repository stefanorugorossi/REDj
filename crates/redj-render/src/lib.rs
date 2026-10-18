//! Offline, deterministic rendering of a mix plan to WAV/MP3 with cue sheet
//! and tags.
//!
//! The render runs the same engine graph as live playback, single-threaded
//! with a fixed block size, so the same plan and sources always produce the
//! same file on the same platform.

mod cue;
mod encode;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use redj_core::{Library, TrackId};
use redj_engine::{EngineConfig, Master, PlanPlayer, TrackAudio};
use redj_plan::{Issue, MixPlan, Timeline, compile};

pub use cue::{TracklistEntry, cue_sheet, tracklist};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("the plan has errors:\n{}", .0.iter().map(|i| format!("  {i}")).collect::<Vec<_>>().join("\n"))]
    Plan(Vec<Issue>),
    #[error(transparent)]
    Media(#[from] redj_media::MediaError),
    #[error("cannot write {path}: {msg}")]
    Write { path: String, msg: String },
    #[error("track {0} is not in the library")]
    MissingTrack(TrackId),
}

/// Output settings.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Output sample rate; `None` uses the most common rate of the tracks.
    pub sample_rate: Option<u32>,
    pub block_size: usize,
    /// Limiter ceiling in dBTP.
    pub ceiling_db: f32,
    /// Two-pass loudness normalization of the whole mix (off by default).
    pub normalize_lufs: Option<f64>,
    /// Also write a 320 kbps CBR MP3 next to the WAV.
    pub mp3: bool,
    /// Write `<name>.cue` and `<name>.plan.json` next to the audio.
    pub sidecars: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            sample_rate: None,
            block_size: 512,
            ceiling_db: -1.0,
            normalize_lufs: None,
            mp3: false,
            sidecars: true,
        }
    }
}

/// What a render produced.
#[derive(Debug, Clone)]
pub struct RenderReport {
    pub wav: PathBuf,
    pub mp3: Option<PathBuf>,
    pub cue: Option<PathBuf>,
    pub plan: Option<PathBuf>,
    pub sample_rate: u32,
    pub duration_sec: f64,
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
    pub tracklist: Vec<TracklistEntry>,
    /// Warnings from plan validation.
    pub warnings: Vec<Issue>,
}

/// Most common sample rate among the plan's tracks (ties: the higher).
pub fn prevailing_rate(timeline: &Timeline, lib: &Library) -> u32 {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for c in &timeline.clips {
        if let Some(t) = lib.get(&c.track) {
            *counts.entry(t.sample_rate).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(rate, n)| (n, rate))
        .map(|(r, _)| r)
        .unwrap_or(44_100)
}

/// Decodes every track used by the timeline (in parallel), converts it to
/// stereo at `sample_rate`, and returns one source per clip.
pub fn load_sources(
    timeline: &Timeline,
    lib: &Library,
    sample_rate: u32,
) -> Result<Vec<Arc<TrackAudio>>, RenderError> {
    let mut ids: Vec<&TrackId> = timeline.clips.iter().map(|c| &c.track).collect();
    ids.sort();
    ids.dedup();
    let loaded: Vec<(TrackId, Arc<TrackAudio>)> = ids
        .par_iter()
        .map(|&id| {
            let info = lib
                .get(id)
                .ok_or_else(|| RenderError::MissingTrack(id.clone()))?;
            let audio = redj_media::decode_file(&info.path)?
                .into_stereo()
                .resampled(sample_rate)?;
            Ok((
                id.clone(),
                Arc::new(TrackAudio::new(
                    id.clone(),
                    sample_rate,
                    info.grid.clone(),
                    audio.samples,
                )),
            ))
        })
        .collect::<Result<_, RenderError>>()?;
    let map: HashMap<TrackId, Arc<TrackAudio>> = loaded.into_iter().collect();
    Ok(timeline
        .clips
        .iter()
        .map(|c| map[&c.track].clone())
        .collect())
}

/// Renders a timeline to interleaved stereo `f32`, compensating the
/// limiter latency. `gain_db` is the master gain before the limiter.
pub fn render_buffer(
    timeline: Arc<Timeline>,
    sources: Vec<Arc<TrackAudio>>,
    cfg: &EngineConfig,
    gain_db: f32,
) -> Vec<f32> {
    let mut player = PlanPlayer::new(timeline, sources, cfg.sample_rate);
    let mut master = Master::new(cfg.sample_rate, cfg.ceiling_db);
    master.set_gain_db(gain_db);
    // Start the smoother at the target gain.
    let latency = master.latency();
    let total = player.end_frame() as usize;
    let mut out = Vec::with_capacity((total + latency) * 2);
    let mut block = vec![0f32; cfg.block_size * 2];
    let mut produced = 0usize;
    while produced < total + latency {
        let frames = cfg.block_size.min(total + latency - produced);
        let buf = &mut block[..frames * 2];
        if player.is_finished() {
            buf.iter_mut().for_each(|s| *s = 0.0);
        } else {
            player.process(buf);
        }
        master.process(buf);
        out.extend_from_slice(buf);
        produced += frames;
    }
    out.drain(..latency * 2);
    out
}

/// Integrated loudness (LUFS) and true peak (dBTP) of interleaved stereo.
pub fn measure(samples: &[f32], sample_rate: u32) -> (f64, f64) {
    use ebur128::{EbuR128, Mode};
    let Ok(mut m) = EbuR128::new(2, sample_rate, Mode::I | Mode::TRUE_PEAK) else {
        return (-70.0, -70.0);
    };
    let _ = m.add_frames_f32(samples);
    let peak = (0..2)
        .filter_map(|c| m.true_peak(c).ok())
        .fold(0f64, f64::max);
    (
        m.loudness_global().unwrap_or(-70.0).max(-70.0),
        20.0 * peak.max(1e-9).log10(),
    )
}

/// Validates, renders and writes a plan. `out` is the WAV path; sidecar
/// files use the same stem.
pub fn render_plan(
    plan: &MixPlan,
    lib: &Library,
    out: &Path,
    opts: &RenderOptions,
) -> Result<RenderReport, RenderError> {
    let compiled = compile(plan, lib);
    let warnings: Vec<Issue> = compiled.warnings().cloned().collect();
    let Some(timeline) = compiled.timeline else {
        return Err(RenderError::Plan(compiled.issues));
    };
    let timeline = Arc::new(timeline);
    let sample_rate = opts
        .sample_rate
        .unwrap_or_else(|| prevailing_rate(&timeline, lib));
    let sources = load_sources(&timeline, lib, sample_rate)?;
    let cfg = EngineConfig {
        sample_rate,
        block_size: opts.block_size,
        ceiling_db: opts.ceiling_db,
    };
    let mut gain_db = 0.0f32;
    let mut samples = render_buffer(timeline.clone(), sources.clone(), &cfg, gain_db);
    if let Some(target) = opts.normalize_lufs {
        let (lufs, _) = measure(&samples, sample_rate);
        if lufs > -70.0 {
            gain_db = (target - lufs).clamp(-24.0, 24.0) as f32;
            tracing::info!("normalizing: {lufs:.1} LUFS, gain {gain_db:+.1} dB");
            samples = render_buffer(timeline.clone(), sources, &cfg, gain_db);
        }
    }
    let (integrated_lufs, true_peak_dbtp) = measure(&samples, sample_rate);
    let list = tracklist(&timeline, lib);
    let title = plan.title.clone().unwrap_or_else(|| "REDj mix".to_owned());

    encode::write_wav(out, &samples, sample_rate)?;
    encode::tag_file(out, &title, &list);
    let mp3 = if opts.mp3 {
        let p = out.with_extension("mp3");
        encode::write_mp3(&p, &samples, sample_rate)?;
        encode::tag_file(&p, &title, &list);
        Some(p)
    } else {
        None
    };
    let (mut cue, mut plan_path) = (None, None);
    if opts.sidecars {
        let p = out.with_extension("cue");
        let file_name = out
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        write_text(&p, &cue_sheet(&title, &file_name, &list))?;
        cue = Some(p);
        let p = out.with_extension("plan.json");
        write_text(
            &p,
            &serde_json::to_string_pretty(plan).expect("plan serializes"),
        )?;
        plan_path = Some(p);
    }
    Ok(RenderReport {
        wav: out.to_path_buf(),
        mp3,
        cue,
        plan: plan_path,
        sample_rate,
        duration_sec: samples.len() as f64 / 2.0 / f64::from(sample_rate),
        integrated_lufs,
        true_peak_dbtp,
        tracklist: list,
        warnings,
    })
}

fn write_text(path: &Path, text: &str) -> Result<(), RenderError> {
    std::fs::write(path, text).map_err(|e| RenderError::Write {
        path: path.display().to_string(),
        msg: e.to_string(),
    })
}
