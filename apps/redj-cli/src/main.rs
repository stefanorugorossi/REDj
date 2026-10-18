//! REDj headless tools.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use redj_analysis::{AnalysisOptions, Cache, default_cache, library_from_dirs};
use redj_core::{Key, Library, Mode, TrackInfo};
use redj_plan::{compile, parse_plan, plan_schema};
use redj_render::{RenderOptions, render_plan};

#[derive(Parser)]
#[command(name = "redj-cli", version, about = "REDj headless tools")]
struct Cli {
    /// Analysis cache directory (default: the user cache directory).
    #[arg(long, global = true)]
    cache: Option<PathBuf>,
    /// Disable the analysis cache.
    #[arg(long, global = true)]
    no_cache: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyze audio files or folders and print the results.
    Analyze {
        /// Files or folders.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Print the full analysis as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the JSON Schema of mix plans.
    Schema,
    /// Check a plan against the tracks in the given folders.
    Validate {
        plan: PathBuf,
        /// Folders (or files) with the plan's tracks.
        #[arg(short, long, required = true)]
        music: Vec<PathBuf>,
    },
    /// Render a plan to WAV (and optionally MP3).
    Render {
        plan: PathBuf,
        /// Folders (or files) with the plan's tracks.
        #[arg(short, long, required = true)]
        music: Vec<PathBuf>,
        /// Output WAV file.
        #[arg(short, long)]
        out: PathBuf,
        /// Also write a 320 kbps MP3.
        #[arg(long)]
        mp3: bool,
        /// Normalize the mix to this integrated loudness (LUFS).
        #[arg(long, allow_negative_numbers = true)]
        normalize: Option<f64>,
        /// Output sample rate (default: the most common rate of the tracks).
        #[arg(long)]
        sample_rate: Option<u32>,
    },
    /// Write synthetic demo tracks (license-free) to a folder.
    Synth {
        dir: PathBuf,
        /// Number of tracks.
        #[arg(short, long, default_value_t = 4)]
        count: usize,
    },
}

fn cache(cli: &Cli) -> Option<Cache> {
    if cli.no_cache {
        None
    } else {
        cli.cache.clone().map(Cache::new).or_else(default_cache)
    }
}

fn load_library(cli: &Cli, dirs: &[PathBuf]) -> Library {
    let (lib, failed) = library_from_dirs(dirs, cache(cli).as_ref(), &AnalysisOptions::default());
    for (p, e) in failed {
        eprintln!("skipped {}: {e}", p.display());
    }
    lib
}

fn summary(t: &TrackInfo) -> String {
    let key = t
        .key
        .map(|k| format!("{} ({})", k.camelot(), k))
        .unwrap_or_else(|| "-".into());
    let sections: Vec<String> = t
        .sections
        .iter()
        .map(|s| format!("{:?}@{}", s.kind, s.start_bar))
        .collect();
    format!(
        "{}  {:7.2} BPM  {:16}  {:5.1} LUFS  {:6.1}s  {}\n    {}",
        t.id,
        t.grid.bpm,
        key,
        t.loudness.integrated_lufs,
        t.duration_sec,
        t.display_name(),
        sections.join(" ")
    )
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,lofty=error".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Analyze { paths, json } => {
            let lib = load_library(&cli, paths);
            if *json {
                let all: Vec<&TrackInfo> = lib.iter().collect();
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else {
                for t in lib.iter() {
                    println!("{}", summary(t));
                }
            }
        }
        Cmd::Schema => println!("{}", serde_json::to_string_pretty(&plan_schema())?),
        Cmd::Validate { plan, music } => {
            let text = std::fs::read_to_string(plan)
                .with_context(|| format!("reading {}", plan.display()))?;
            let plan = parse_plan(&text).map_err(|i| anyhow::anyhow!("{i}"))?;
            let lib = load_library(&cli, music);
            let c = compile(&plan, &lib);
            for i in &c.issues {
                println!("{i}");
            }
            match &c.timeline {
                Some(t) => println!(
                    "OK: {} tracks, {:.1} minutes",
                    t.clips.len(),
                    t.duration_sec() / 60.0
                ),
                None => bail!("the plan has errors"),
            }
        }
        Cmd::Render {
            plan,
            music,
            out,
            mp3,
            normalize,
            sample_rate,
        } => {
            let text = std::fs::read_to_string(plan)
                .with_context(|| format!("reading {}", plan.display()))?;
            let plan = parse_plan(&text).map_err(|i| anyhow::anyhow!("{i}"))?;
            let lib = load_library(&cli, music);
            let opts = RenderOptions {
                sample_rate: *sample_rate,
                normalize_lufs: *normalize,
                mp3: *mp3,
                ..RenderOptions::default()
            };
            let rep = render_plan(&plan, &lib, out, &opts)?;
            for w in &rep.warnings {
                eprintln!("{w}");
            }
            println!(
                "wrote {} ({:.1} min, {} Hz, {:.1} LUFS, {:.1} dBTP)",
                rep.wav.display(),
                rep.duration_sec / 60.0,
                rep.sample_rate,
                rep.integrated_lufs,
                rep.true_peak_dbtp
            );
            if let Some(m) = &rep.mp3 {
                println!("wrote {}", m.display());
            }
            for (i, t) in rep.tracklist.iter().enumerate() {
                let s = t.start_sec as u64;
                println!("  {:2}. {:02}:{:02}  {}", i + 1, s / 60, s % 60, t.title);
            }
        }
        Cmd::Synth { dir, count } => {
            std::fs::create_dir_all(dir)?;
            let presets = [
                (122.0, Key::new(9, Mode::Minor)),
                (124.0, Key::new(4, Mode::Minor)),
                (126.0, Key::new(0, Mode::Major)),
                (128.0, Key::new(7, Mode::Major)),
                (125.0, Key::new(2, Mode::Minor)),
                (127.0, Key::new(11, Mode::Minor)),
            ];
            for i in 0..*count {
                let (bpm, key) = presets[i % presets.len()];
                let spec = redj_testkit::SynthSpec::club(bpm, key).with_seed(i as u64 + 1);
                let path = dir.join(format!(
                    "demo-{:02}-{bpm:.0}bpm-{}.wav",
                    i + 1,
                    key.camelot()
                ));
                redj_testkit::render(&spec).write_wav(&path)?;
                println!("wrote {}", path.display());
            }
        }
    }
    Ok(())
}
