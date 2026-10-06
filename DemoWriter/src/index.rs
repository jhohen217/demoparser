//! A whole-file frame index plus the structural facts the planner and verifier need.

use crate::frame::*;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

pub struct DemoIndex {
    pub file_len: u64,
    pub header_file_info_offset: u32,
    pub header_spawn_groups_offset: u32,
    pub frames: Vec<FrameRef>,
    pub full_packets: Vec<usize>,
    /// First `DEM_Packet`/`DEM_FullPacket` with a non-negative tick. Everything before it
    /// is the initialization region — note that tick alone does not identify it, since
    /// animation and recovery frames also carry tick -1.
    pub first_gameplay: Option<usize>,
    /// Last `DEM_Packet` in the whole file. Frames after it that are not trailer records
    /// are the tick-final block (17 MB of animation in one of the four fixtures).
    pub last_packet: Option<usize>,
    pub stop: Option<usize>,
    pub spawn_groups_trailer: Option<usize>,
    pub file_info: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CensusRow {
    pub cmd: u32,
    pub group: &'static str,
    pub frames: u64,
    pub payload_bytes: u64,
    pub tick_min: Option<i32>,
    pub tick_max: Option<i32>,
}

impl DemoIndex {
    pub fn build(demo: &[u8]) -> Result<Self> {
        let (frames, end) = scan(demo)?;
        if end != demo.len() {
            bail!(
                "frame scan stopped at {} but the file is {} bytes; the stream is not \
                 frame-aligned",
                end,
                demo.len()
            );
        }

        let header_file_info_offset =
            u32::from_le_bytes(demo[8..12].try_into().context("short header truncated")?);
        let header_spawn_groups_offset =
            u32::from_le_bytes(demo[12..16].try_into().context("short header truncated")?);

        let full_packets = frames
            .iter()
            .filter(|f| f.cmd == CMD_FULL_PACKET)
            .map(|f| f.index)
            .collect::<Vec<_>>();

        let first_gameplay = frames
            .iter()
            .find(|f| (f.cmd == CMD_PACKET || f.cmd == CMD_FULL_PACKET) && f.tick() >= 0)
            .map(|f| f.index);
        let last_packet = frames
            .iter()
            .rev()
            .find(|f| f.cmd == CMD_PACKET)
            .map(|f| f.index);
        let stop = frames.iter().find(|f| f.cmd == CMD_STOP).map(|f| f.index);
        let spawn_groups_trailer = stop.and_then(|s| {
            frames
                .iter()
                .skip(s)
                .find(|f| f.cmd == CMD_SPAWN_GROUPS)
                .map(|f| f.index)
        });
        let file_info = frames
            .iter()
            .find(|f| f.cmd == CMD_FILE_INFO)
            .map(|f| f.index);

        Ok(DemoIndex {
            file_len: demo.len() as u64,
            header_file_info_offset,
            header_spawn_groups_offset,
            frames,
            full_packets,
            first_gameplay,
            last_packet,
            stop,
            spawn_groups_trailer,
            file_info,
        })
    }

    /// Frames after the last `DEM_Packet` that are not trailer records — the tick-final
    /// block. Empty in three of the four known fixtures.
    pub fn tail_block(&self) -> &[FrameRef] {
        let Some(last_packet) = self.last_packet else {
            return &[];
        };
        let start = last_packet + 1;
        let end = self.stop.unwrap_or(self.frames.len()).max(start);
        &self.frames[start..end]
    }

    pub fn tail_block_bytes(&self) -> u64 {
        self.tail_block().iter().map(|f| f.total_len()).sum()
    }

    pub fn last_tick(&self) -> i32 {
        self.frames
            .iter()
            .rev()
            .map(|f| f.tick())
            .find(|t| *t >= 0)
            .unwrap_or(0)
    }

    /// Greatest full packet whose tick is <= `tick`.
    pub fn checkpoint_at_or_before(&self, tick: i32) -> Option<usize> {
        self.full_packets
            .iter()
            .copied()
            .filter(|i| {
                let t = self.frames[*i].tick();
                t >= 0 && t <= tick
            })
            .max_by_key(|i| self.frames[*i].tick())
    }

    pub fn census(&self) -> Vec<CensusRow> {
        let boundary = self.last_packet.unwrap_or(usize::MAX);
        let mut rows: BTreeMap<(u32, &'static str), CensusRow> = BTreeMap::new();
        for frame in &self.frames {
            let group = if frame.index <= boundary {
                "body"
            } else {
                "tail"
            };
            let row = rows.entry((frame.cmd, group)).or_insert(CensusRow {
                cmd: frame.cmd,
                group,
                frames: 0,
                payload_bytes: 0,
                tick_min: None,
                tick_max: None,
            });
            row.frames += 1;
            row.payload_bytes += frame.payload_len as u64;
            let tick = frame.tick();
            row.tick_min = Some(row.tick_min.map_or(tick, |m: i32| m.min(tick)));
            row.tick_max = Some(row.tick_max.map_or(tick, |m: i32| m.max(tick)));
        }
        rows.into_values().collect()
    }

    /// Checks that hold for a source demo and must hold for anything we write.
    pub fn structural_report(&self) -> Vec<Check> {
        let mut checks = Vec::new();
        let frame_at = |offset: u32| {
            self.frames
                .iter()
                .find(|f| f.frame_offset == offset as u64)
                .map(|f| f.name())
        };

        checks.push(Check::new(
            "frame scan consumes the file exactly",
            true,
            format!("{} frames, {} bytes", self.frames.len(), self.file_len),
        ));

        let fi_ok = self.file_info.map(|i| self.frames[i].frame_offset)
            == Some(self.header_file_info_offset as u64);
        checks.push(Check::new(
            "header bytes 8..12 point at DEM_FileInfo",
            fi_ok,
            format!(
                "offset {} -> {}",
                self.header_file_info_offset,
                frame_at(self.header_file_info_offset).unwrap_or("nothing")
            ),
        ));

        let sg_ok = self
            .spawn_groups_trailer
            .map(|i| self.frames[i].frame_offset)
            == Some(self.header_spawn_groups_offset as u64);
        checks.push(Check::new(
            "header bytes 12..16 point at post-stop DEM_SpawnGroups",
            sg_ok,
            format!(
                "offset {} -> {}",
                self.header_spawn_groups_offset,
                frame_at(self.header_spawn_groups_offset).unwrap_or("nothing")
            ),
        ));

        let stop_count = self.frames.iter().filter(|f| f.cmd == CMD_STOP).count();
        checks.push(Check::new(
            "exactly one DEM_Stop",
            stop_count == 1,
            format!("found {stop_count}"),
        ));

        let last_is_file_info = self
            .frames
            .last()
            .map(|f| f.cmd == CMD_FILE_INFO)
            .unwrap_or(false);
        checks.push(Check::new(
            "DEM_FileInfo is the last frame",
            last_is_file_info,
            self.frames
                .last()
                .map(|f| f.name().to_string())
                .unwrap_or_else(|| "no frames".to_string()),
        ));

        let stop_before_trailer = match (self.stop, self.spawn_groups_trailer, self.file_info) {
            (Some(s), Some(sg), Some(fi)) => s < sg && sg < fi,
            (Some(s), None, Some(fi)) => s < fi,
            _ => false,
        };
        checks.push(Check::new(
            "order is DEM_Stop -> DEM_SpawnGroups -> DEM_FileInfo",
            stop_before_trailer,
            format!(
                "stop={:?} spawngroups={:?} fileinfo={:?}",
                self.stop, self.spawn_groups_trailer, self.file_info
            ),
        ));

        checks.push(Check::new(
            "at least one DEM_FullPacket checkpoint",
            !self.full_packets.is_empty(),
            format!("{} checkpoints", self.full_packets.len()),
        ));

        checks
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

impl Check {
    fn new(name: &'static str, passed: bool, detail: String) -> Self {
        Check {
            name,
            passed,
            detail,
        }
    }
}
