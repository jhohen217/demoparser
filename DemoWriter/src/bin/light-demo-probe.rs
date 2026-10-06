//! Experimental same-schema static light transplant. No game hooks are required by this executable.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{io::Write, path::{Path, PathBuf}};
#[derive(Parser)]
#[command(about = "Experimental same-schema static omni/barn/rect light transplant")]
struct Cli { #[command(subcommand)] command: Command }
#[derive(Subcommand)]
enum Command {
    Snapshot { demo: PathBuf, entity: i32, tick: i32, output: PathBuf },
    Inject { target: PathBuf, donor: PathBuf, snapshot: PathBuf, output: PathBuf },
}
fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)
        .with_context(|| format!("create {} (output must be new)", path.display()))?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Snapshot { demo, entity, tick, output } => {
            let value = demo_writer::light_snapshot(&std::fs::read(demo)?, entity, tick)?;
            write_new(&output, &serde_json::to_vec_pretty(&value)?)?;
        }
        Command::Inject { target, donor, snapshot, output } => {
            let snapshot = serde_json::from_slice(&std::fs::read(snapshot)?)?;
            let (bytes, report) = demo_writer::light_inject(&std::fs::read(target)?, &std::fs::read(donor)?, &snapshot)?;
            write_new(&output, &bytes)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}
