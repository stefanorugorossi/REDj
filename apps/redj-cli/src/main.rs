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
    /// List the Ollama models and whether they support tool calling.
    Models {
        #[command(flatten)]
        ollama: OllamaArgs,
    },
    /// Build a set from a music folder with the AI (or the built-in
    /// planner) and render it.
    Set {
        /// Folders (or files) with the music.
        #[arg(required = true)]
        music: Vec<PathBuf>,
        /// Output WAV file.
        #[arg(short, long)]
        out: PathBuf,
        /// Approximate set length in minutes.
        #[arg(long)]
        minutes: Option<f64>,
        /// Maximum number of tracks.
        #[arg(long)]
        max_tracks: Option<usize>,
        /// Wishes for the AI, e.g. "dark and driving".
        #[arg(long)]
        notes: Option<String>,
        /// Use the built-in planner instead of the AI.
        #[arg(long)]
        no_ai: bool,
        /// Enable the model's thinking mode (slower, sometimes better).
        #[arg(long)]
        think: bool,
        /// Also write a 320 kbps MP3.
        #[arg(long)]
        mp3: bool,
        /// Only plan: write the plan JSON here and skip rendering.
        #[arg(long)]
        plan_only: Option<PathBuf>,
        #[command(flatten)]
        ollama: OllamaArgs,
    },
    /// Run the MCP server on stdin/stdout (for Claude Code / Desktop).
    Mcp {
        /// Folders (or files) with the music.
        #[arg(short, long, required = true)]
        music: Vec<PathBuf>,
        /// Folder where `render_mix` writes files (rendering disabled
        /// without it).
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct OllamaArgs {
    /// Ollama model.
    #[arg(long, default_value = "qwen3.6:35b-a3b-q4_K_M")]
    model: String,
    /// Ollama URL.
    #[arg(long, default_value = redj_ai::DEFAULT_URL)]
    ollama: String,
    /// Timeout of one model request, in seconds.
    #[arg(long, default_value_t = 900)]
    timeout: u64,
    /// Context window (tokens).
    #[arg(long, default_value_t = 32768)]
    num_ctx: u32,
}

fn print_report(rep: &redj_render::RenderReport) {
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

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

fn print_event(e: &redj_ai::AgentEvent) {
    use redj_ai::AgentEvent::*;
    match e {
        Request { turn } => eprintln!("[ai] turn {turn}: waiting for the model..."),
        Response {
            secs,
            prompt_tokens,
            output_tokens,
            ..
        } => eprintln!(
            "[ai]   {secs:.0}s ({prompt_tokens} prompt tokens, {output_tokens} generated)"
        ),
        Thinking(t) => eprintln!("[ai]   thinking: {}", t.lines().next().unwrap_or("")),
        Text(t) => eprintln!("[ai]   says: {}", t.trim()),
        ToolCall { name, arguments } => eprintln!("[ai]   -> {name} {arguments}"),
        ToolResult { name, ok, preview } => {
            eprintln!(
                "[ai]   <- {name} {}: {preview}",
                if *ok { "ok" } else { "ERROR" }
            )
        }
        Retry { error, .. } => eprintln!("[ai]   retrying after error: {error}"),
    }
}

/// Plans a set with the AI; `None` when the AI is unavailable or failed
/// (the caller falls back to the built-in planner).
fn ai_plan(
    session: &std::sync::Arc<redj_control::Session>,
    req: &redj_ai::SetRequest,
    o: &OllamaArgs,
    think: bool,
) -> Result<bool> {
    let rt = runtime()?;
    rt.block_on(async {
        let client =
            redj_ai::OllamaClient::new(&o.ollama, std::time::Duration::from_secs(o.timeout));
        let caps = match client.capabilities(&o.model).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("AI unavailable ({e}); using the built-in planner");
                return Ok(false);
            }
        };
        if !caps.iter().any(|c| c == "tools") {
            eprintln!(
                "model {} does not support tool calling; using the built-in planner",
                o.model
            );
            return Ok(false);
        }
        let cfg = redj_ai::AgentConfig {
            model: o.model.clone(),
            think: caps.iter().any(|c| c == "thinking").then_some(think),
            num_ctx: o.num_ctx,
            ..redj_ai::AgentConfig::default()
        };
        eprintln!(
            "[ai] planning with {} (this can take several minutes on CPU)",
            o.model
        );
        match redj_ai::plan_set(session.clone(), client, cfg, req, |e| print_event(&e)).await {
            Ok(out) if session.accepted_plan().is_some() => {
                eprintln!(
                    "[ai] plan accepted after {} turns, {} tool calls",
                    out.turns, out.tool_calls
                );
                Ok(true)
            }
            Ok(out) => {
                eprintln!(
                    "[ai] the model stopped without an accepted plan: {}",
                    out.text.trim()
                );
                Ok(false)
            }
            Err(e) => {
                eprintln!("[ai] failed: {e}; using the built-in planner");
                Ok(false)
            }
        }
    })
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
            print_report(&render_plan(&plan, &lib, out, &opts)?);
        }
        Cmd::Models { ollama } => {
            let rt = runtime()?;
            let client =
                redj_ai::OllamaClient::new(&ollama.ollama, std::time::Duration::from_secs(30));
            let models = rt.block_on(client.list_models())?;
            for m in models {
                println!(
                    "{:40} {:6.1} GB  tools: {:3}  thinking: {}",
                    m.name,
                    m.size_bytes as f64 / 1e9,
                    if m.supports_tools() { "yes" } else { "no" },
                    if m.supports_thinking() { "yes" } else { "no" }
                );
            }
        }
        Cmd::Set {
            music,
            out,
            minutes,
            max_tracks,
            notes,
            no_ai,
            think,
            mp3,
            plan_only,
            ollama,
        } => {
            let lib = load_library(&cli, music);
            if lib.is_empty() {
                bail!("no playable tracks found");
            }
            eprintln!("{} tracks analyzed", lib.len());
            let session = std::sync::Arc::new(redj_control::Session::new(lib.clone()));
            let req = redj_ai::SetRequest {
                minutes: *minutes,
                max_tracks: *max_tracks,
                notes: notes.clone(),
            };
            let by_ai = !*no_ai && ai_plan(&session, &req, ollama, *think)?;
            let plan = match session.accepted_plan() {
                Some(p) if by_ai => p,
                _ => {
                    let p = redj_control::auto_plan(
                        &lib,
                        &redj_control::AutoPlanOptions {
                            max_tracks: *max_tracks,
                            target_minutes: *minutes,
                            ..redj_control::AutoPlanOptions::default()
                        },
                    );
                    eprintln!("built-in planner: {} tracks", p.entries.len());
                    p
                }
            };
            if let Some(path) = plan_only {
                std::fs::write(path, serde_json::to_string_pretty(&plan)?)?;
                println!("wrote {}", path.display());
                return Ok(());
            }
            let opts = RenderOptions {
                mp3: *mp3,
                ..RenderOptions::default()
            };
            print_report(&render_plan(&plan, &lib, out, &opts)?);
        }
        Cmd::Mcp { music, out_dir } => {
            let lib = load_library(&cli, music);
            eprintln!("redj MCP server: {} tracks", lib.len());
            let mut session = redj_control::Session::new(lib);
            if let Some(d) = out_dir {
                std::fs::create_dir_all(d)?;
                session = session.with_output_dir(d);
            }
            let rt = runtime()?;
            rt.block_on(redj_mcp::serve_stdio(std::sync::Arc::new(session)))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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
