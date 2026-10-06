//! Read-only exact-lifetime export of resident network fields, including class-baseline values.
use anyhow::{Context, Result};
use demo_writer::EntityFieldAuditQuery;
use serde::Deserialize;

#[derive(Deserialize)]
struct QueryFile {
    queries: Vec<EntityFieldAuditQuery>,
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        anyhow::bail!("usage: audit-entity-fields <demo> <queries.json>");
    }
    let demo = std::fs::read(&args[0]).with_context(|| format!("read demo {}", args[0]))?;
    let queries: QueryFile = serde_json::from_slice(
        &std::fs::read(&args[1]).with_context(|| format!("read query file {}", args[1]))?,
    )?;
    let rows = demo_writer::audit_restore_entity_fields(&demo, queries.queries)?;
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
