//! Read-only Source 2 entity create/delete command inventory.
use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 1 {
        bail!("usage: inspect-entity-events <demo>");
    }
    let demo = std::fs::read(&args[0]).context("read demo")?;
    println!("{}", demo_writer::inspect_restore_entity_events(&demo)?);
    Ok(())
}
