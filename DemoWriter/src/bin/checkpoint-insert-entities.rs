//! Add entities that are live in the sequential stream but absent from a DEM_FullPacket
//! checkpoint, so they survive a seek.
use anyhow::{bail, Context, Result};
use std::io::Write;

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 && !(args.len() == 5 && args[4] == "--packet") {
        bail!("usage: checkpoint-insert-entities <in.dem> <out.dem> <tick> <entity,entity,...> [--packet]");
    }
    let demo = std::fs::read(&args[0]).context("read demo")?;
    let tick = args[2].parse::<i32>().context("checkpoint tick")?;
    let ids = args[3]
        .split(',')
        .map(|v| v.trim().parse::<i32>())
        .collect::<Result<Vec<_>, _>>()
        .context("entity list")?;
    let (out, inserted) = if args.len() == 5 {
        demo_writer::materialise_packet_entities(&demo, tick, &ids)?
    } else {
        demo_writer::insert_checkpoint_entities(&demo, tick, &ids)?
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args[1])
        .with_context(|| format!("create {}", args[1]))?;
    file.write_all(&out)?;
    for (id, class, serial) in inserted {
        println!("inserted entity {id} class {class} serial {serial}");
    }
    println!("{} -> {} bytes", demo.len(), out.len());
    Ok(())
}
