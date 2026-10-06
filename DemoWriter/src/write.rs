//! Raw-splice writer: copies selected source frames byte-for-byte and synthesises only the
//! records that must describe the new container.

use crate::frame::*;
use crate::index::DemoIndex;
use crate::plan::{MetadataPolicy, Policies, SpawnGroupsPolicy, TrimPlan};
use anyhow::{bail, Context, Result};
use csgoproto::{CDemoFileHeader, CDemoFileInfo, CDemoFullPacket};
use prost::Message;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub struct WriteOutcome {
    pub output_bytes: u64,
    pub file_info_offset: u32,
    pub spawn_groups_offset: u32,
    pub playback_ticks: i32,
    pub playback_frames: i32,
    pub playback_time: f32,
    pub temp_path: PathBuf,
}

/// The checkpoint's string tables, re-encoded as a standalone `CDemoStringTables` payload.
///
/// CS2 does not take `instancebaseline` from inside a `DEM_FullPacket` the way this
/// repository's parser does. Given only the match's opening string tables it fails with
/// `GetClassBaseline: FindStringIndex(191-CWeaponMAC10) failed` as soon as it reaches the
/// checkpoint, because entries created during the skipped middle of the match are absent.
/// Emitting the checkpoint's own tables as a `DEM_StringTables` frame — the channel the
/// engine reads at load — gives it the state it expects.
///
/// Not every full packet carries them: measured across the fixtures, `string_table` is
/// populated on some checkpoints and absent on others, with no visible pattern. So walk
/// backwards from the checkpoint to the most recent one that actually has tables, and
/// report which tick it came from — an older snapshot still names every entity class seen
/// up to that point, which is what `GetClassBaseline` is looking for.
pub fn string_tables_at_or_before(
    demo: &[u8],
    index: &DemoIndex,
    checkpoint_frame: usize,
) -> Result<Option<(Vec<u8>, i32)>> {
    let candidates = index
        .full_packets
        .iter()
        .copied()
        .filter(|i| *i <= checkpoint_frame)
        .collect::<Vec<_>>();
    for frame_index in candidates.into_iter().rev() {
        if let Some(bytes) = full_packet_string_tables(demo, index, frame_index)? {
            if !bytes.is_empty() {
                return Ok(Some((bytes, index.frames[frame_index].tick())));
            }
        }
    }
    Ok(None)
}

pub fn full_packet_string_tables(
    demo: &[u8],
    index: &DemoIndex,
    checkpoint_frame: usize,
) -> Result<Option<Vec<u8>>> {
    let frame = &index.frames[checkpoint_frame];
    let raw = frame.payload(demo);
    let decoded = if frame.compressed {
        snap::raw::Decoder::new()
            .decompress_vec(raw)
            .context("could not decompress the checkpoint full packet")?
    } else {
        raw.to_vec()
    };
    let full_packet =
        CDemoFullPacket::decode(&decoded[..]).context("could not decode the checkpoint")?;
    match full_packet.string_table {
        Some(tables) => {
            let mut buf = Vec::with_capacity(tables.encoded_len());
            tables.encode(&mut buf)?;
            Ok(Some(buf))
        }
        None => Ok(None),
    }
}

/// Build the `CDemoFileInfo` for the output. Source unknown fields are not preserved —
/// prost drops them — which is acceptable only because every fixture's FileInfo decodes
/// completely into the four known fields.
fn build_file_info(
    demo: &[u8],
    index: &DemoIndex,
    plan: &TrimPlan,
    policies: Policies,
) -> Result<(CDemoFileInfo, i32, i32, f32)> {
    let mut info = match index.file_info {
        Some(i) => {
            let frame = &index.frames[i];
            if frame.compressed {
                // Not observed in any fixture; refuse rather than guess.
                bail!("source DEM_FileInfo is compressed, which this version cannot rewrite");
            }
            CDemoFileInfo::decode(frame.payload(demo))
                .context("could not decode the source CDemoFileInfo")?
        }
        None => CDemoFileInfo::default(),
    };

    let shift = policies.tick_shift(plan.checkpoint_tick);
    let playback_ticks = match policies.metadata {
        MetadataPolicy::Absolute => plan.body_last_tick - shift,
        MetadataPolicy::Window => plan.body_last_tick - plan.checkpoint_tick,
    };
    let playback_frames = plan.packet_frames as i32;
    // Exactly ticks / tickrate, which reproduces all four fixtures bit-for-bit.
    let playback_time = playback_ticks as f32 / policies.tickrate;

    info.playback_ticks = Some(playback_ticks);
    info.playback_frames = Some(playback_frames);
    info.playback_time = Some(playback_time);
    if let Some(game_info) = info.game_info.as_mut() {
        if let Some(cs) = game_info.cs.as_mut() {
            cs.round_start_ticks
                .retain(|tick| *tick >= plan.checkpoint_tick && *tick <= plan.body_last_tick);
        }
    }

    Ok((info, playback_ticks, playback_frames, playback_time))
}

pub fn estimate_size(
    demo: &[u8],
    index: &DemoIndex,
    plan: &TrimPlan,
    policies: Policies,
) -> Result<u64> {
    let (info, _, _, _) = build_file_info(demo, index, plan, policies)?;
    let info_len = info.encoded_len() as u32;
    let shift = policies.tick_shift(plan.checkpoint_tick);
    let tick = (plan.body_last_tick - shift).max(0) as u32;

    let mut total = HEADER_LEN as u64 + plan.retained_bytes();
    if policies.sync_string_tables {
        if let Some((tables, _)) = string_tables_at_or_before(demo, index, plan.checkpoint_frame)? {
            let len = tables.len() as u32;
            let ck = (plan.checkpoint_tick - shift).max(0) as u32;
            total += frame_header_len(CMD_STRING_TABLES, false, ck, len) as u64 + len as u64;
        }
    }
    total += frame_header_len(CMD_STOP, false, tick, 0) as u64;
    match policies.spawn_groups {
        SpawnGroupsPolicy::Omit => {}
        SpawnGroupsPolicy::Empty => {
            total += frame_header_len(CMD_SPAWN_GROUPS, false, tick, 0) as u64;
        }
        SpawnGroupsPolicy::Preserve => {
            let (len, compressed) = match index.spawn_groups_trailer {
                Some(i) => (index.frames[i].payload_len, index.frames[i].compressed),
                None => (0, false),
            };
            total += frame_header_len(CMD_SPAWN_GROUPS, compressed, tick, len) as u64 + len as u64;
        }
    }
    total += frame_header_len(CMD_FILE_INFO, false, tick, info_len) as u64 + info_len as u64;

    // Rewriting ticks changes varint widths, so account for every frame whose header moves.
    for (position, frame_index) in plan.selected.iter().enumerate() {
        let frame = &index.frames[*frame_index];
        if frame.tick() < 0 {
            continue;
        }
        let in_startup = position >= plan.bootstrap_count && position < plan.prefix_count();
        let new_tick = if policies.align_startup && in_startup {
            ((plan.checkpoint_tick - 1).max(0) - shift).max(0) as u32
        } else if shift != 0 {
            (frame.tick() - shift).max(0) as u32
        } else {
            continue;
        };
        let before = frame_header_len(
            frame.cmd,
            frame.compressed,
            frame.tick_raw,
            frame.payload_len,
        );
        let after = frame_header_len(frame.cmd, frame.compressed, new_tick, frame.payload_len);
        total = total + after as u64 - before as u64;
    }
    Ok(total)
}

struct WriteSummary {
    output_bytes: u64,
    file_info_offset: u32,
    spawn_groups_offset: u32,
    playback_ticks: i32,
    playback_frames: i32,
    playback_time: f32,
}

/// Build a parser-only window without touching the filesystem.
pub fn write_trimmed_bytes(
    demo: &[u8],
    index: &DemoIndex,
    plan: &TrimPlan,
    policies: Policies,
) -> Result<Vec<u8>> {
    let capacity = estimate_size(demo, index, plan, policies)?.min(usize::MAX as u64) as usize;
    let mut output = Cursor::new(Vec::with_capacity(capacity));
    write_trimmed_into(demo, index, plan, policies, &mut output)?;
    Ok(output.into_inner())
}

fn write_trimmed_into<W: Write + Seek>(
    demo: &[u8],
    index: &DemoIndex,
    plan: &TrimPlan,
    policies: Policies,
    out: &mut W,
) -> Result<WriteSummary> {
    let (info, playback_ticks, playback_frames, playback_time) =
        build_file_info(demo, index, plan, policies)?;
    let shift = policies.tick_shift(plan.checkpoint_tick);
    let tick = (plan.body_last_tick - shift).max(0) as u32;

    let mut offset: u64 = 0;

    // Short header with placeholder pointers; patched once the trailer offsets are known.
    out.write_all(MAGIC)?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(&0u32.to_le_bytes())?;
    offset += HEADER_LEN as u64;

    let synthetic_tables = if policies.sync_string_tables {
        string_tables_at_or_before(demo, index, plan.checkpoint_frame)?
    } else {
        None
    };
    if let Some((bytes, tick)) = &synthetic_tables {
        println!(
            "string tables      {} bytes, from the full packet at tick {}",
            bytes.len(),
            tick
        );
    }
    let synthetic_tables = synthetic_tables.map(|(bytes, _)| bytes);

    for (position, index_of_frame) in plan.selected.iter().enumerate() {
        // Hand the engine the checkpoint's string tables at the end of the signon block,
        // before any gameplay frame.
        if position == plan.bootstrap_count {
            if let Some(tables) = &synthetic_tables {
                let ck = (plan.checkpoint_tick - shift).max(0) as u32;
                out.write_all(&frame_header(
                    CMD_STRING_TABLES,
                    false,
                    ck,
                    tables.len() as u32,
                ))?;
                out.write_all(tables)?;
                offset += frame_header_len(CMD_STRING_TABLES, false, ck, tables.len() as u32)
                    as u64
                    + tables.len() as u64;
            }
        }
        let frame = &index.frames[*index_of_frame];
        // Startup-burst frames get a new outer tick so playback starts at the checkpoint
        // rather than ten minutes of dead air earlier. Payload bytes are untouched.
        let in_startup = position >= plan.bootstrap_count && position < plan.prefix_count();
        let new_tick = if frame.tick() < 0 {
            None // initialization frames keep tick -1
        } else if policies.align_startup && in_startup {
            Some(((plan.checkpoint_tick - 1).max(0) - shift).max(0) as u32)
        } else if shift != 0 {
            Some((frame.tick() - shift).max(0) as u32)
        } else {
            None
        };

        // The FileHeader carries the anchor that maps demo ticks onto server ticks, so it
        // has to move by the same amount the timeline did.
        let rewritten_header = if shift != 0 && frame.cmd == CMD_FILE_HEADER && !frame.compressed {
            match CDemoFileHeader::decode(frame.payload(demo)) {
                Ok(mut header) => {
                    if let Some(start) = header.server_start_tick {
                        header.server_start_tick = Some(start + shift);
                        let mut buf = Vec::with_capacity(header.encoded_len());
                        header.encode(&mut buf)?;
                        Some(buf)
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        } else {
            None
        };

        match (new_tick, rewritten_header) {
            (None, None) => {
                out.write_all(frame.bytes(demo))?;
                offset += frame.total_len();
            }
            (tick_override, payload_override) => {
                let tick = tick_override.unwrap_or(frame.tick_raw);
                let payload: &[u8] = match &payload_override {
                    Some(bytes) => bytes,
                    None => frame.payload(demo),
                };
                let header = frame_header(frame.cmd, frame.compressed, tick, payload.len() as u32);
                out.write_all(&header)?;
                out.write_all(payload)?;
                offset += header.len() as u64 + payload.len() as u64;
            }
        }
    }

    // Synthetic stop.
    out.write_all(&frame_header(CMD_STOP, false, tick, 0))?;
    offset += frame_header_len(CMD_STOP, false, tick, 0) as u64;

    // Spawn groups trailer.
    let spawn_groups_offset = offset;
    match policies.spawn_groups {
        SpawnGroupsPolicy::Omit => {}
        SpawnGroupsPolicy::Empty => {
            out.write_all(&frame_header(CMD_SPAWN_GROUPS, false, tick, 0))?;
            offset += frame_header_len(CMD_SPAWN_GROUPS, false, tick, 0) as u64;
        }
        SpawnGroupsPolicy::Preserve => match index.spawn_groups_trailer {
            Some(i) => {
                let frame = &index.frames[i];
                let payload = frame.payload(demo);
                out.write_all(&frame_header(
                    CMD_SPAWN_GROUPS,
                    frame.compressed,
                    tick,
                    frame.payload_len,
                ))?;
                out.write_all(payload)?;
                offset +=
                    frame_header_len(CMD_SPAWN_GROUPS, frame.compressed, tick, frame.payload_len)
                        as u64
                        + frame.payload_len as u64;
            }
            None => {
                out.write_all(&frame_header(CMD_SPAWN_GROUPS, false, tick, 0))?;
                offset += frame_header_len(CMD_SPAWN_GROUPS, false, tick, 0) as u64;
            }
        },
    }

    // FileInfo trailer.
    let file_info_offset = offset;
    let mut info_bytes = Vec::with_capacity(info.encoded_len());
    info.encode(&mut info_bytes)?;
    out.write_all(&frame_header(
        CMD_FILE_INFO,
        false,
        tick,
        info_bytes.len() as u32,
    ))?;
    out.write_all(&info_bytes)?;
    offset += frame_header_len(CMD_FILE_INFO, false, tick, info_bytes.len() as u32) as u64
        + info_bytes.len() as u64;

    if offset > u32::MAX as u64 {
        bail!(
            "output would be {} bytes, past the u32 range the short-header pointers use",
            offset
        );
    }

    // Patch the two short-header pointers.
    out.seek(SeekFrom::Start(8))?;
    out.write_all(&(file_info_offset as u32).to_le_bytes())?;
    let sg_pointer = match policies.spawn_groups {
        SpawnGroupsPolicy::Omit => 0,
        _ => spawn_groups_offset as u32,
    };
    out.write_all(&sg_pointer.to_le_bytes())?;
    out.flush()?;

    Ok(WriteSummary {
        output_bytes: offset,
        file_info_offset: file_info_offset as u32,
        spawn_groups_offset: sg_pointer,
        playback_ticks,
        playback_frames,
        playback_time,
    })
}

pub fn write_trimmed(
    demo: &[u8],
    index: &DemoIndex,
    plan: &TrimPlan,
    policies: Policies,
    destination: &Path,
) -> Result<WriteOutcome> {
    let dir = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).ok();
    let temp_path = dir.join(format!(
        "{}.partial-{}",
        destination
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "output.dem".to_string()),
        std::process::id()
    ));
    let file = File::create(&temp_path)
        .with_context(|| format!("could not create {}", temp_path.display()))?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let summary = match write_trimmed_into(demo, index, plan, policies, &mut out) {
        Ok(summary) => summary,
        Err(error) => {
            drop(out);
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
    };
    let file = out.into_inner().context("could not flush the output")?;
    file.sync_all()?;

    Ok(WriteOutcome {
        output_bytes: summary.output_bytes,
        file_info_offset: summary.file_info_offset,
        spawn_groups_offset: summary.spawn_groups_offset,
        playback_ticks: summary.playback_ticks,
        playback_frames: summary.playback_frames,
        playback_time: summary.playback_time,
        temp_path,
    })
}
