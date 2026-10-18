//! REDj GUI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(name = "redj", version, about = "REDj: AI DJ console")]
struct Cli {
    /// Music folders to add to the library.
    music: Vec<PathBuf>,
    /// Load two tracks and start playing (for a quick look).
    #[arg(long)]
    demo: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,lofty=error,wgpu=error".into()),
        )
        .init();
    let cli = Cli::parse();
    if let Err(e) = redj_ui::run(redj_ui::UiOptions {
        music: cli.music,
        demo: cli.demo,
    }) {
        eprintln!("REDj failed to start: {e}");
        std::process::exit(1);
    }
}
