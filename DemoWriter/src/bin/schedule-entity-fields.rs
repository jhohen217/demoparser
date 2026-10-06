//! Apply explicit, lifetime-guarded scalar writes to entity deltas in a demo.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::Write;

#[derive(Deserialize)]
struct Schedule {
    writes: Vec<WriteRow>,
}

#[derive(Deserialize)]
struct WriteRow {
    tick: i32,
    entity_index: i32,
    class_id: u32,
    serial: u32,
    field_path: Vec<i32>,
    value: Scalar,
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
enum Scalar {
    U32(u32),
    U64(u64),
    I32(i32),
    F32(f32),
    // Raw unsigned simulation-time ticks; the library checks the field decoder.
    TimeTicks(u32),
    String(String),
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let allow_create_writes = args.iter().any(|arg| arg == "--allow-create-writes");
    let create_only = args.iter().any(|arg| arg == "--create-only");
    if allow_create_writes && create_only {
        bail!("--create-only already enables create writes; do not combine the flags");
    }
    args.retain(|arg| arg != "--allow-create-writes");
    args.retain(|arg| arg != "--create-only");
    if args.len() != 3 {
        bail!("usage: schedule-entity-fields [--allow-create-writes | --create-only] <schema-demo-in> <schedule.json> <demo-out>");
    }
    let source = std::fs::read(&args[0]).context("read schema demo")?;
    let schedule: Schedule =
        serde_json::from_slice(&std::fs::read(&args[1]).context("read schedule JSON")?)
            .context("decode schedule JSON")?;
    let writes = schedule
        .writes
        .into_iter()
        .map(|row| demo_writer::ScheduledFieldWrite {
            tick: row.tick,
            entity_id: row.entity_index,
            class_id: row.class_id,
            serial: row.serial,
            field_path: row.field_path,
            value: match row.value {
                Scalar::U32(value) => demo_writer::ScheduledScalar::U32(value),
                Scalar::U64(value) => demo_writer::ScheduledScalar::U64(value),
                Scalar::I32(value) => demo_writer::ScheduledScalar::I32(value),
                Scalar::F32(value) => demo_writer::ScheduledScalar::F32(value),
                Scalar::TimeTicks(value) => demo_writer::ScheduledScalar::TimeTicks(value),
                Scalar::String(value) => demo_writer::ScheduledScalar::String(value),
            },
        })
        .collect();
    let (patched, count) = if create_only {
        demo_writer::apply_scheduled_entity_writes_create_only(&source, writes)?
    } else {
        demo_writer::apply_scheduled_entity_writes_with_create(
            &source,
            writes,
            allow_create_writes,
        )?
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[2])
        .context("create diagnostic output")?;
    file.write_all(&patched)?;
    println!(
        "applied {count} scheduled entity-field writes; {} -> {} bytes",
        source.len(),
        patched.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_ticks_json_preserves_u32_wire_values() {
        for value in [0, 23356, 114171, u32::MAX] {
            let scalar: Scalar =
                serde_json::from_str(&format!("{{\"type\":\"timeticks\",\"value\":{value}}}"))
                    .unwrap();
            assert!(matches!(scalar, Scalar::TimeTicks(actual) if actual == value));
        }
    }

    #[test]
    fn time_ticks_json_rejects_non_u32_values() {
        for value in ["-1", "4294967296", "true", "23356.0", "\"23356\"", "null"] {
            assert!(serde_json::from_str::<Scalar>(&format!(
                "{{\"type\":\"timeticks\",\"value\":{value}}}"
            ))
            .is_err());
        }
    }
}
