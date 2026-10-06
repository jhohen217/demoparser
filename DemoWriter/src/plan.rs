//! Turning "I want round 12" into an exact list of source frames to copy.

use crate::frame::*;
use crate::index::DemoIndex;
use anyhow::{bail, Result};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationPolicy {
    /// Everything, including a tick-final trailing block if the selection reaches it.
    Keep,
    /// Animation inside the body, but never the tick-final block.
    BodyOnly,
    /// No animation frames at all.
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapPolicy {
    /// Every frame before the first gameplay frame.
    Full,
    /// Only the records needed to interpret later packets; drops the tick -1 animation and
    /// recovery pre-roll, which is 5.1 MB in one of the four known fixtures.
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPolicy {
    /// FileInfo describes the original timeline: final tick as retained.
    Absolute,
    /// FileInfo describes only the retained window.
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnGroupsPolicy {
    Preserve,
    Empty,
    Omit,
}

impl fmt::Display for AnimationPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AnimationPolicy::Keep => "keep",
            AnimationPolicy::BodyOnly => "body-only",
            AnimationPolicy::Drop => "drop",
        })
    }
}
impl fmt::Display for BootstrapPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BootstrapPolicy::Full => "full",
            BootstrapPolicy::Required => "required",
        })
    }
}
impl fmt::Display for MetadataPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MetadataPolicy::Absolute => "absolute",
            MetadataPolicy::Window => "window",
        })
    }
}
impl fmt::Display for SpawnGroupsPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SpawnGroupsPolicy::Preserve => "preserve",
            SpawnGroupsPolicy::Empty => "empty",
            SpawnGroupsPolicy::Omit => "omit",
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Policies {
    pub animation: AnimationPolicy,
    pub bootstrap: BootstrapPolicy,
    pub metadata: MetadataPolicy,
    pub spawn_groups: SpawnGroupsPolicy,
    pub tickrate: f32,
    /// Also copy the demo's opening gameplay burst — every frame from the first gameplay
    /// frame through the first `DEM_FullPacket`.
    ///
    /// CS2 will not finish loading without it. A clip built from signon records plus a
    /// mid-match checkpoint reaches "waiting for first spawn group" and hangs there
    /// forever: the message that makes the client load the map's spawn group is sent in
    /// that opening burst, not in the signon block and not in later full packets.
    pub include_startup: bool,
    /// Emit the checkpoint's string tables as a `DEM_StringTables` frame just before the
    /// checkpoint. Without this CS2 fails on `GetClassBaseline` for every entity class
    /// first seen after the opening tick.
    pub sync_string_tables: bool,
    /// Relabel the startup burst's outer frame tick so it sits immediately before the
    /// checkpoint instead of at tick 1.
    ///
    /// CS2 drives its playback clock from the outer frame tick. Left at tick 1, a clip
    /// whose content starts at tick 38401 plays ten minutes of frozen opening-tick state
    /// before reaching anything — the viewer has to seek manually. Only the frame header
    /// changes; payload bytes are still copied verbatim.
    pub align_startup: bool,
    /// Rebase the clip's timeline to start at tick 1, the way a real demo does.
    ///
    /// Every outer frame tick has `checkpoint - 1` subtracted from it, and the same amount
    /// is added to `DEM_FileHeader.server_start_tick` so the engine's demo-tick ↔ game-tick
    /// mapping still resolves to the original server ticks the payloads carry. Payload
    /// bytes are untouched; only frame headers and that one header field change.
    pub rebase_ticks: bool,
}

impl Policies {
    /// Amount subtracted from every outer tick. Zero when rebasing is off.
    pub fn tick_shift(&self, checkpoint_tick: i32) -> i32 {
        if self.rebase_ticks {
            (checkpoint_tick - 1).max(0)
        } else {
            0
        }
    }
}

impl Default for Policies {
    fn default() -> Self {
        Policies {
            animation: AnimationPolicy::Keep,
            bootstrap: BootstrapPolicy::Full,
            metadata: MetadataPolicy::Absolute,
            spawn_groups: SpawnGroupsPolicy::Preserve,
            tickrate: 64.0,
            include_startup: true,
            sync_string_tables: true,
            align_startup: true,
            rebase_ticks: true,
        }
    }
}

/// Records that must survive `--bootstrap required`.
fn is_required_init(cmd: u32) -> bool {
    !matches!(
        cmd,
        CMD_ANIMATION_DATA | CMD_ANIMATION_HEADER | CMD_RECOVERY
    )
}

pub struct TrimPlan {
    pub requested_start_tick: i32,
    pub requested_end_tick: i32,
    pub checkpoint_frame: usize,
    pub checkpoint_tick: i32,
    pub body_last_frame: usize,
    pub body_last_tick: i32,
    /// Source frame indices to copy, in order: bootstrap, startup burst, then body.
    pub selected: Vec<usize>,
    pub bootstrap_count: usize,
    pub bootstrap_bytes: u64,
    pub startup_count: usize,
    pub startup_bytes: u64,
    pub body_bytes: u64,
    pub packet_frames: usize,
    pub animation_frames_kept: usize,
    pub animation_bytes_kept: u64,
    pub animation_bytes_dropped: u64,
    pub tail_block_bytes_dropped: u64,
    pub checkpoint_padding_ticks: i32,
    pub checkpoint_padding_bytes: u64,
    pub warnings: Vec<String>,
}

impl TrimPlan {
    pub fn retained_bytes(&self) -> u64 {
        self.bootstrap_bytes + self.startup_bytes + self.body_bytes
    }
    /// Number of frames written before the checkpoint — bootstrap plus startup burst.
    pub fn prefix_count(&self) -> usize {
        self.bootstrap_count + self.startup_count
    }
}

pub fn plan_trim(
    index: &DemoIndex,
    start_tick: i32,
    end_tick: i32,
    policies: Policies,
) -> Result<TrimPlan> {
    if end_tick < start_tick {
        bail!("end tick {end_tick} is before start tick {start_tick}");
    }
    let first_gameplay = index
        .first_gameplay
        .ok_or_else(|| anyhow::anyhow!("no gameplay frames found in the source demo"))?;
    let last_packet = index
        .last_packet
        .ok_or_else(|| anyhow::anyhow!("no DEM_Packet frames found in the source demo"))?;

    // Prefer a strictly earlier snapshot so the ordinary packet immediately before a
    // same-tick full packet (which can carry round-start/audio events) remains in the body.
    let checkpoint_frame = index
        .checkpoint_at_or_before(start_tick.saturating_sub(1))
        .or_else(|| index.checkpoint_at_or_before(start_tick))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no DEM_FullPacket checkpoint at or before tick {start_tick}; the earliest \
             checkpoint is at tick {}",
                index
                    .full_packets
                    .first()
                    .map(|i| index.frames[*i].tick())
                    .unwrap_or(-1)
            )
        })?;
    let checkpoint_tick = index.frames[checkpoint_frame].tick();

    let mut warnings = Vec::new();

    // Body end: the last DEM_Packet inside the window, then any non-packet frames that
    // belong to the same window and are not part of the tick-final block. Expressed in
    // frame indices, never as "tick <= end", so a last-round selection cannot swallow a
    // trailing animation block.
    let last_packet_in_window = index.frames[checkpoint_frame..=last_packet]
        .iter()
        .rev()
        .find(|f| f.cmd == CMD_PACKET && f.tick() >= 0 && f.tick() <= end_tick)
        .map(|f| f.index);
    let Some(mut body_last_frame) = last_packet_in_window else {
        bail!(
            "no DEM_Packet between the checkpoint at tick {checkpoint_tick} and tick \
             {end_tick}"
        );
    };

    let mut i = body_last_frame + 1;
    while i < index.frames.len() {
        let frame = &index.frames[i];
        if is_trailer_cmd(frame.cmd) || frame.cmd == CMD_PACKET {
            break;
        }
        if frame.tick() > end_tick {
            break;
        }
        let in_tail_block = i > last_packet;
        if in_tail_block && policies.animation != AnimationPolicy::Keep {
            break;
        }
        body_last_frame = i;
        i += 1;
    }
    // The clip's real final tick: the last retained frame that carries a real tick.
    let body_last_tick = index.frames[checkpoint_frame..=body_last_frame]
        .iter()
        .rev()
        .map(|f| f.tick())
        .find(|t| *t >= 0)
        .unwrap_or(end_tick);

    // Bootstrap.
    let mut selected = Vec::new();
    let mut bootstrap_bytes = 0u64;
    for frame in &index.frames[..first_gameplay] {
        let keep = match policies.bootstrap {
            BootstrapPolicy::Full => true,
            BootstrapPolicy::Required => is_required_init(frame.cmd),
        };
        if keep {
            selected.push(frame.index);
            bootstrap_bytes += frame.total_len();
        }
    }
    let bootstrap_count = selected.len();

    // Startup burst: the opening gameplay frames through the first full packet. Without
    // these CS2 never loads the map's spawn group and sits on the loading screen.
    let mut startup_count = 0usize;
    let mut startup_bytes = 0u64;
    // playback_frames counts every DEM_Packet in the output, startup burst included.
    let mut packet_frames = 0usize;
    if policies.include_startup {
        // Up to, but not including, the demo's first full packet. The spawn-group load
        // CS2 needs is in the opening DEM_Packet; copying the opening full packet too
        // would make it — rather than the checkpoint — the first snapshot in the output,
        // which leaves a mid-match clip with opening-tick entity state.
        let first_full_packet = index
            .full_packets
            .first()
            .copied()
            .unwrap_or(first_gameplay);
        if first_full_packet <= checkpoint_frame {
            for frame in &index.frames[first_gameplay..first_full_packet] {
                if is_trailer_cmd(frame.cmd) {
                    continue;
                }
                if is_animation(frame.cmd) && policies.animation == AnimationPolicy::Drop {
                    continue;
                }
                if frame.cmd == CMD_PACKET {
                    packet_frames += 1;
                }
                selected.push(frame.index);
                startup_count += 1;
                startup_bytes += frame.total_len();
            }
        }
    }

    // Body.
    let mut body_bytes = 0u64;
    let mut animation_frames_kept = 0usize;
    let mut animation_bytes_kept = 0u64;
    let mut animation_bytes_dropped = 0u64;
    for frame in &index.frames[checkpoint_frame..=body_last_frame] {
        if is_trailer_cmd(frame.cmd) {
            // The source's own stop/trailer never gets copied at its original position.
            continue;
        }
        if is_animation(frame.cmd) {
            let in_tail_block = frame.index > last_packet;
            let keep = match policies.animation {
                AnimationPolicy::Keep => true,
                AnimationPolicy::BodyOnly => !in_tail_block,
                AnimationPolicy::Drop => false,
            };
            if !keep {
                animation_bytes_dropped += frame.total_len();
                continue;
            }
            animation_frames_kept += 1;
            animation_bytes_kept += frame.total_len();
        }
        if frame.cmd == CMD_PACKET {
            packet_frames += 1;
        }
        selected.push(frame.index);
        body_bytes += frame.total_len();
    }

    // What the tick-final block would have cost had the selection reached it.
    let tail_block_bytes_dropped = if body_last_frame >= last_packet {
        match policies.animation {
            AnimationPolicy::Keep => 0,
            _ => index.tail_block_bytes(),
        }
    } else {
        0
    };
    if tail_block_bytes_dropped > 0 {
        warnings.push(format!(
            "dropped a {:.1} MB tick-final block of {} frames after the last DEM_Packet \
             (animation policy {})",
            tail_block_bytes_dropped as f64 / 1e6,
            index.tail_block().len(),
            policies.animation
        ));
    }

    let checkpoint_padding_ticks = start_tick - checkpoint_tick;
    let checkpoint_padding_bytes = index.frames[checkpoint_frame..]
        .iter()
        .take_while(|f| f.tick() < start_tick || f.tick() < 0)
        .map(|f| f.total_len())
        .sum();

    if checkpoint_padding_ticks > 0 {
        warnings.push(format!(
            "clip starts {} ticks ({:.1} s) before the requested tick because the nearest \
             checkpoint is at tick {}",
            checkpoint_padding_ticks,
            checkpoint_padding_ticks as f32 / policies.tickrate,
            checkpoint_tick
        ));
    }

    Ok(TrimPlan {
        requested_start_tick: start_tick,
        requested_end_tick: end_tick,
        checkpoint_frame,
        checkpoint_tick,
        body_last_frame,
        body_last_tick,
        selected,
        bootstrap_count,
        bootstrap_bytes,
        startup_count,
        startup_bytes,
        body_bytes,
        packet_frames,
        animation_frames_kept,
        animation_bytes_kept,
        animation_bytes_dropped,
        tail_block_bytes_dropped,
        checkpoint_padding_ticks,
        checkpoint_padding_bytes,
        warnings,
    })
}
