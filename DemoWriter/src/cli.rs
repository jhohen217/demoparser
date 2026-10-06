// demo-writer — cut one round, or a contiguous range of rounds, out of a CS2 demo.
//
// Validity is claimed in layers, and they are not the same claim:
//   structural (this tool's own verifier) < parser-valid (this repo's parser) < playback.
// Nothing here can promise the last one; only CS2 can.

mod bitwriter;
mod boundary;
mod frame;
mod index;
mod plan;
pub mod poserecipe;
mod props;
mod retarget;
mod rounds;
mod serverinfo;
mod suppress;
mod write;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser as ClapParser, Subcommand, ValueEnum};
use frame::*;
use index::DemoIndex;
use memmap2::Mmap;
use plan::*;
use rayon::prelude::*;
use std::fs::File;
use std::io::Read;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(ClapParser)]
#[command(name = "demo-writer", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Frame census and structural check of a demo.
    Inspect {
        demo: PathBuf,
        /// Also list every checkpoint tick.
        #[arg(long)]
        checkpoints: bool,
    },
    /// Read packet net_Tick values to relate SourceTV frame ticks to server ticks.
    NetTicks {
        demo: PathBuf,
        #[arg(long, default_value_t = i32::MIN)]
        from: i32,
        #[arg(long, default_value_t = i32::MAX)]
        to: i32,
    },
    /// List rounds. Requires a full parse of the demo, which is the slow part.
    Rounds { demo: PathBuf },
    /// Report where a player's weapon attack overlay restarts.
    ///
    /// The firing animation is not a clip of its own: it is a permanent overlay whose playback
    /// time restarts on each trigger pull. This finds it the way it was found in the first place,
    /// by looking for a sampler whose time runs backwards on a tick the player fired, and is the
    /// check for whether a suppression actually removed the animation.
    Overlay {
        demo: PathBuf,
        /// Shooter pawn entity index.
        #[arg(long)]
        entity: i32,
        /// Read the shot ticks from this demo instead of from the one being checked.
        ///
        /// Required for checking an edited demo: a suppression removes the discharges, so the
        /// file under test has no shots to compare against and every check passes for the wrong
        /// reason. The unedited source supplies the ticks the animation should be absent from.
        #[arg(long)]
        shots_from: Option<PathBuf>,
        #[arg(long, default_value_t = i32::MIN)]
        from: i32,
        #[arg(long, default_value_t = i32::MAX)]
        to: i32,
    },
    /// Report attack or reload input a demo carries for one player.
    ///
    /// CS2 records a trigger pull twice: as a held bit in the command's button state, and as a
    /// subtick step naming the button and the moment inside the tick it went down. A suppression
    /// that clears only one of them leaves the client firing a shot per press, so this reports
    /// both and makes the claim "the input is clean" checkable rather than assumed.
    Inputs {
        demo: PathBuf,
        /// Shooter pawn entity index.
        #[arg(long)]
        entity: i32,
        /// Button to inspect. Attack remains the default for suppression checks.
        #[arg(long, value_enum, default_value_t = InputButton::Attack)]
        button: InputButton,
        #[arg(long, default_value_t = i32::MIN)]
        from: i32,
        #[arg(long, default_value_t = i32::MAX)]
        to: i32,
    },
    /// Structural verification only — usable on a source demo or on our own output.
    Verify { demo: PathBuf },
    /// Show the demo's svc_ServerInfo — the message CS2's version gate reads.
    Serverinfo { demo: PathBuf },
    /// Experimental: point a demo at a different map.
    Retarget {
        demo: PathBuf,
        #[arg(long)]
        map: String,
        /// Replacement world name for the old map's prefab spawn groups (skybox). Dropped
        /// when not given.
        #[arg(long)]
        sky: Option<String>,
        /// Workshop published file id, for pointing the demo at a workshop copy.
        #[arg(long)]
        addon: Option<String>,
        /// Keep the old map's resource manifests instead of clearing them.
        #[arg(long)]
        keep_manifests: bool,
        #[arg(long, short)]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Remove one player's shots from a demo, writing an edited copy.
    ///
    /// Drops the fire-bullets temp entity, and optionally the weapon sound, for the named
    /// player over a tick range. Entity state — magazine, shots-fired, recoil — is delta
    /// encoded and is NOT edited, so run `--list` first and compare the result against a
    /// props sample before trusting the output.
    Suppress {
        demo: PathBuf,
        /// List the demo's discharges instead of writing anything.
        #[arg(long)]
        list: bool,
        /// Shooter, as the entity index printed by --list.
        #[arg(long)]
        entity: Option<i32>,
        /// First tick to suppress, inclusive. Defaults to the start of the demo.
        #[arg(long)]
        from: Option<i32>,
        /// Last tick to suppress, inclusive. Defaults to the end of the demo.
        #[arg(long)]
        to: Option<i32>,
        /// Leave the gunshot audible; only the tracer and muzzle flash go.
        #[arg(long)]
        keep_sound: bool,
        /// Drop only the shot's messages, leaving entity state exactly as recorded.
        #[arg(long)]
        effects_only: bool,
        /// Hold the view at this pitch, in degrees, while spinning. Level by default.
        #[arg(long)]
        spin_pitch: Option<f32>,
        /// Which aim routine runs, 0-7. The execute function dispatches on this to five
        /// different solves; a recording sits on 2 almost always.
        #[arg(long)]
        aim_mode: Option<u32>,
        /// Set the aim task's three weights, as `<a>:<b>:<c>`, each 0-255 or `-` to leave alone.
        #[arg(long, value_parser = parse_aim_weights)]
        aim_weights: Option<[Option<u32>; 3]>,
        /// Nudge the aim away from what was recorded, as `<first>:<second>` in -1..1.
        ///
        /// Preferred over `--aim`: holding the field at a constant contorts the model, because the
        /// rig is solving toward a fixed offset it never sits at in play.
        #[arg(long, value_parser = parse_aim_hold)]
        aim_offset: Option<(f32, f32)>,
        /// Hold the aim task at a fixed setting, as `<first>:<second>` in -1..1.
        ///
        /// Mapped inside the range a recording uses rather than written raw: the full sixteen bit
        /// span crashes the client once playback reaches the edited ticks.
        #[arg(long, value_parser = parse_aim_hold)]
        aim: Option<(f32, f32)>,
        /// Sweep one of the aim task's two values, 0 or 1, to see what it steers.
        #[arg(long)]
        aim_sweep: Option<u8>,
        /// Spin this player's view, in degrees per second. Negative turns the other way.
        ///
        /// Rewrites `m_angEyeAngles` to sweep steadily instead of following what they really did.
        /// The value written is whichever direction they were recorded looking that is closest to
        /// the one wanted, so the error is a fraction of a degree.
        #[arg(long)]
        spin: Option<f32>,
        /// Substitute one animation clip for another in this player's pose, as `from:to`.
        /// Repeatable.
        ///
        /// This is what makes a weapon swap look right: the recorded pose keeps the old grip
        /// unless the clips themselves change. Only works where both weapons build the same task
        /// list — a clip index can be rewritten in place, but a sampler that is not there cannot
        /// be added.
        #[arg(long = "swap-clip", value_parser = parse_clip_swap)]
        swap_clip: Vec<(u32, u32)>,
        /// Append a clip to this player's pose, as `<clip>[:<weight>[:<mask>]]`.
        ///
        /// Unlike --swap-clip this adds tasks rather than rewriting a value, so it works on a
        /// recipe that never sampled the clip at all. A sample is appended, then a blend consuming
        /// the recipe's previous result and the new sample, which makes the blend the new result.
        ///
        /// Weight defaults to 255, fully replacing the pose — which is what you want for a first
        /// test, because a partial blend of a wrong clip and a broken append look alike. Mask names
        /// an entry in the skeleton's bone mask list and confines the clip to those bones.
        #[arg(long = "append-clip", value_parser = parse_append_clip)]
        append_clip: Option<(u32, u32, Option<u32>)>,
        /// Put a sticker on a weapon, as `<weapon entity>:<slot>:<sticker id>`. Repeatable.
        ///
        /// The econ attribute list is networked with its own length, so slots beyond the four the
        /// game offers are just a longer list. Whether the client draws them all is the open
        /// question this exists to answer.
        #[arg(long = "add-sticker", value_parser = parse_sticker)]
        add_sticker: Vec<(i32, u32, u32)>,
        /// Repaint a weapon, as `<weapon entity>:<paint kit index>`. Repeatable.
        ///
        /// The kit is a raw float in the item's econ attributes, so any value can be written —
        /// unlike the item definition, which the client validates against those attributes and
        /// crashes on.
        #[arg(long = "swap-paint", value_parser = parse_paint_swap)]
        swap_paint: Vec<(i32, f32)>,
        /// Weapon entity index this player should never be seen switching to. Repeatable.
        ///
        /// Dropping the switch leaves the previous weapon in hand until the next switch, so
        /// `--skip-weapon` on the middle of three turns "rifle, pistol, knife" into "rifle,
        /// knife". Entity indices come from `fields --only ActiveWeapon`.
        #[arg(long = "skip-weapon")]
        skip_weapon: Vec<i32>,
        /// Remove the recorded user commands entirely.
        ///
        /// CS2's TrueView re-simulates a demo's first person view from these, so a viewer who
        /// forces `cl_demo_predict 2` sees shots reconstructed locally even when the demo holds
        /// none. Without commands there is nothing to reconstruct from. TrueView is off by default
        /// for a demo whose build does not match the client, so this is only needed when the
        /// output must hold up against a viewer who turns it on deliberately.
        #[arg(long)]
        strip_usercmds: bool,
        /// Write the demo even though suppressed shots killed someone, leaving them to fall
        /// over unshot. A death cannot be undone: the recording stops simulating a dead player.
        #[arg(long)]
        allow_orphan_deaths: bool,
        /// Write the demo even though suppressed shots wounded someone, leaving health to drop
        /// for no visible reason.
        #[arg(long)]
        allow_orphan_damage: bool,
        #[arg(long, short)]
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Census the entity fields written to one entity over a tick range.
    ///
    /// This is what a suppression needs before it can touch entity state: the fields a shot
    /// actually writes, named by the demo's own serializers rather than assumed.
    Fields {
        demo: PathBuf,
        /// Entity index, as printed by `suppress --list`.
        #[arg(long)]
        entity: i32,
        #[arg(long)]
        from: i32,
        #[arg(long)]
        to: i32,
        /// Only report fields whose name contains this text.
        #[arg(long)]
        only: Option<String>,
        /// Include the exact value bits (bit count:hex bytes).
        #[arg(long)]
        raw: bool,
    },
    /// List every serializer field of one class with its path, from this demo's own class table.
    ClassPaths {
        demo: PathBuf,
        /// Class name, e.g. CKnife.
        #[arg(long)]
        class: String,
    },
    /// Dump the animation recipe payload per tick, labelled, for correlation analysis.
    Pose {
        demo: PathBuf,
        #[arg(long)]
        entity: i32,
        #[arg(long)]
        from: i32,
        #[arg(long)]
        to: i32,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Resolve every pawn's active-weapon handle to its recorded entity class.
    WeaponTimeline {
        demo: PathBuf,
        #[arg(long, short)]
        output: PathBuf,
    },
    /// Test decal-to-shot attribution: for every decal, how far it lies off each shot's line.
    Decals {
        demo: PathBuf,
        /// Shooter entity index, as printed by `suppress --list`.
        #[arg(long)]
        entity: i32,
        /// Ticks after the shot within which a decal may be attributed to it.
        #[arg(long, default_value_t = 4)]
        window: i32,
    },
    /// Dump the damage game events a demo carries, to see what a suppressed shot leaves behind.
    Events {
        demo: PathBuf,
        #[arg(long)]
        from: i32,
        #[arg(long)]
        to: i32,
    },
    /// Decode and re-encode every inner packet stream, checking the bytes come back
    /// identical. Proves the bit writer before it is trusted with an edit.
    Roundtrip {
        demo: PathBuf,
        /// Stop after this many packet frames.
        #[arg(long, default_value_t = 2000)]
        limit: usize,
        /// Include the nested packet inside each DEM_FullPacket checkpoint.
        #[arg(long)]
        include_full_packets: bool,
    },
    /// Dump the string tables carried by a DEM_FullPacket.
    Tables {
        demo: PathBuf,
        /// Checkpoint tick; defaults to the first full packet.
        #[arg(long)]
        tick: Option<i32>,
        /// Report whether this class id appears in instancebaseline.
        #[arg(long)]
        class: Option<String>,
    },
    /// Sample player properties at one tick, for comparing a clip against its source.
    Props {
        demo: PathBuf,
        #[arg(long)]
        tick: i32,
        /// Comma-separated property names; defaults to a small fidelity set.
        #[arg(long)]
        props: Option<String>,
        /// Use the parser's single-threaded path.
        #[arg(long)]
        single_threaded: bool,
    },
    /// Write a trimmed demo.
    Trim(TrimArgs),
}

#[derive(Args)]
struct TrimArgs {
    demo: PathBuf,
    /// Single round number, as reported by `rounds`.
    #[arg(long, group = "selection")]
    round: Option<i32>,
    /// Inclusive round range, e.g. 12-14.
    #[arg(long, group = "selection")]
    rounds: Option<String>,
    /// Separate round clips to emit in one process, e.g. 3,7,12. Round boundaries are resolved
    /// with one full parse of the source instead of reparsing it once per output.
    #[arg(long, group = "selection", requires = "output_directory")]
    round_list: Option<String>,
    /// Raw tick range, e.g. 45000-51000. Skips the full parse; expert/debug option.
    #[arg(long, group = "selection")]
    ticks: Option<String>,
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Destination for --round-list clips. Files retain the standard <source>_r<N>.dem names.
    #[arg(long, requires = "round_list", conflicts_with = "output")]
    output_directory: Option<PathBuf>,
    /// Ticks to keep after round_end when round_officially_ended is unavailable.
    #[arg(long, default_value_t = DEFAULT_TAIL_TICKS)]
    tail_ticks: i32,
    /// Include the round's buy/freeze phase. By default the logical clip starts at freeze end.
    #[arg(long)]
    include_buy_time: bool,
    #[arg(long, value_enum, default_value_t = AnimationArg::Keep)]
    animation: AnimationArg,
    #[arg(long, value_enum, default_value_t = BootstrapArg::Full)]
    bootstrap: BootstrapArg,
    #[arg(long = "metadata-policy", value_enum, default_value_t = MetadataArg::Absolute)]
    metadata_policy: MetadataArg,
    #[arg(long = "spawn-groups", value_enum, default_value_t = SpawnGroupsArg::Preserve)]
    spawn_groups: SpawnGroupsArg,
    #[arg(long, default_value_t = 64.0)]
    tickrate: f32,
    /// Omit the demo's opening gameplay burst. CS2 will not finish loading without it —
    /// for experiments only.
    #[arg(long)]
    no_startup: bool,
    /// Do not re-emit the checkpoint's string tables. CS2 fails on GetClassBaseline
    /// without them — for experiments only.
    #[arg(long)]
    no_string_table_sync: bool,
    /// Leave the startup burst at its original tick. Playback then begins with a long
    /// stretch of dead air — for experiments only.
    #[arg(long)]
    no_align_startup: bool,
    /// Keep the source's absolute tick numbering instead of rebasing the clip to tick 1.
    /// The clip then reports the full match length as its duration.
    #[arg(long)]
    no_rebase: bool,
    /// Plan and report without writing anything.
    #[arg(long)]
    dry_run: bool,
    /// Overwrite an existing destination.
    #[arg(long)]
    force: bool,
    /// Skip the reparse of the output with this repository's parser.
    #[arg(long)]
    no_parse_check: bool,
    /// Write the trim report as JSON.
    #[arg(long)]
    json_report: Option<PathBuf>,
    /// Write one <source>_r<N>.json report per --round-list clip in this directory.
    #[arg(long, requires = "round_list", conflicts_with = "json_report")]
    json_report_directory: Option<PathBuf>,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum AnimationArg {
    Keep,
    BodyOnly,
    Drop,
}
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum BootstrapArg {
    Full,
    Required,
}
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum MetadataArg {
    Absolute,
    Window,
}
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum SpawnGroupsArg {
    Preserve,
    Empty,
    Omit,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum InputButton {
    Attack,
    Reload,
}

impl InputButton {
    fn mask(self) -> u64 {
        match self {
            Self::Attack => 1,
            Self::Reload => 1 << 13,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Attack => "attack",
            Self::Reload => "reload",
        }
    }
}

enum DemoData {
    Mapped(Mmap),
    Decompressed(Vec<u8>),
}

impl Deref for DemoData {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Mapped(mapped) => mapped,
            Self::Decompressed(bytes) => bytes,
        }
    }
}

fn map_demo(path: &Path) -> Result<DemoData> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".dem.gz") || lower.ends_with(".gz") {
        let file =
            File::open(path).with_context(|| format!("could not open {}", path.display()))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut bytes = Vec::new();
        decoder
            .read_to_end(&mut bytes)
            .with_context(|| format!("could not decompress {}", path.display()))?;
        return Ok(DemoData::Decompressed(bytes));
    }
    if lower.ends_with(".dem.zst") || lower.ends_with(".zst") {
        let file =
            File::open(path).with_context(|| format!("could not open {}", path.display()))?;
        let mut decoder = zstd::stream::Decoder::new(file)
            .with_context(|| format!("could not open zstd stream {}", path.display()))?;
        let mut bytes = Vec::new();
        decoder
            .read_to_end(&mut bytes)
            .with_context(|| format!("could not decompress {}", path.display()))?;
        return Ok(DemoData::Decompressed(bytes));
    }
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    // SAFETY: the demo is read-only for the lifetime of the process; we never write to it.
    unsafe { Mmap::map(&file) }
        .map(DemoData::Mapped)
        .with_context(|| format!("could not map {}", path.display()))
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn cmd_net_ticks(path: &Path, from: i32, to: i32) -> Result<()> {
    use prost::Message as _;
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let mut count = 0usize;
    println!("demo_tick,net_tick");
    for frame in &index.frames {
        if frame.cmd != CMD_PACKET || frame.tick() < from || frame.tick() > to {
            continue;
        }
        if let Some(messages) = suppress::packet_messages(&demo, frame)? {
            for message in messages {
                if message.msg_type == 4 {
                    let tick = csgoproto::CnetMsgTick::decode(message.payload.as_slice())?;
                    if let Some(value) = tick.tick {
                        println!("{},{}", frame.tick(), value);
                        count += 1;
                    }
                }
            }
        }
    }
    anyhow::ensure!(count > 0, "no net_Tick messages in selected packet range");
    Ok(())
}

fn cmd_inspect(path: &Path, show_checkpoints: bool) -> Result<()> {
    let demo = map_demo(path)?;
    let started = Instant::now();
    let index = DemoIndex::build(&demo)?;

    println!("file    {}", path.display());
    println!(
        "size    {} ({} bytes)   frames {}   scan {:.2}s",
        human(index.file_len),
        index.file_len,
        index.frames.len(),
        started.elapsed().as_secs_f64()
    );
    println!("ticks   last tick {}", index.last_tick());
    println!();

    println!(
        "{:<24} {:<5} {:>8} {:>15} {:>10} {:>10}",
        "command", "group", "frames", "payload bytes", "tick min", "tick max"
    );
    for row in index.census() {
        println!(
            "{:<24} {:<5} {:>8} {:>15} {:>10} {:>10}",
            cmd_name(row.cmd),
            row.group,
            row.frames,
            row.payload_bytes,
            row.tick_min.unwrap_or(0),
            row.tick_max.unwrap_or(0),
        );
    }
    println!();

    let init_end = index.first_gameplay.unwrap_or(0);
    let init_bytes: u64 = index.frames[..init_end].iter().map(|f| f.total_len()).sum();
    println!(
        "init region       {} frames, {} (ends at frame {})",
        init_end,
        human(init_bytes),
        init_end
    );
    let tail = index.tail_block();
    println!(
        "tick-final block  {} frames, {}",
        tail.len(),
        human(index.tail_block_bytes())
    );
    println!(
        "checkpoints       {} (first tick {}, last tick {})",
        index.full_packets.len(),
        index
            .full_packets
            .first()
            .map(|i| index.frames[*i].tick())
            .unwrap_or(-1),
        index
            .full_packets
            .last()
            .map(|i| index.frames[*i].tick())
            .unwrap_or(-1),
    );
    if index.full_packets.len() > 1 {
        let ticks: Vec<i32> = index
            .full_packets
            .iter()
            .map(|i| index.frames[*i].tick())
            .collect();
        let intervals: Vec<i32> = ticks.windows(2).map(|w| w[1] - w[0]).collect();
        let min = intervals.iter().min().copied().unwrap_or(0);
        let max = intervals.iter().max().copied().unwrap_or(0);
        println!(
            "checkpoint gap    {}",
            if min == max {
                format!("{min} ticks, uniform")
            } else {
                format!("{min}..{max} ticks, irregular")
            }
        );
    }
    if show_checkpoints {
        let ticks: Vec<String> = index
            .full_packets
            .iter()
            .map(|i| index.frames[*i].tick().to_string())
            .collect();
        println!("checkpoint ticks  {}", ticks.join(", "));
    }
    println!();

    print_checks(&index);
    Ok(())
}

fn print_checks(index: &DemoIndex) -> bool {
    let checks = index.structural_report();
    let mut all_passed = true;
    println!("structural checks");
    for check in &checks {
        println!(
            "  [{}] {:<52} {}",
            if check.passed { "ok" } else { "FAIL" },
            check.name,
            check.detail
        );
        all_passed &= check.passed;
    }
    all_passed
}

fn cmd_verify(path: &Path) -> Result<()> {
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    println!("file  {}", path.display());
    println!("size  {} ({} bytes)", human(index.file_len), index.file_len);
    println!();
    if !print_checks(&index) {
        bail!("structural verification failed");
    }
    println!("\nstructurally valid");
    Ok(())
}

fn cmd_rounds(path: &Path) -> Result<()> {
    let demo = map_demo(path)?;
    let started = Instant::now();
    let rounds = rounds::parse_rounds(&demo)?;
    println!(
        "parsed {} rounds in {:.1}s\n",
        rounds.len(),
        started.elapsed().as_secs_f64()
    );
    println!(
        "{:>5} {:>10} {:>11} {:>10} {:>12}  {:<4} {}",
        "round", "start", "freeze end", "end", "officially", "win", "reason"
    );
    for round in &rounds {
        println!(
            "{:>5} {:>10} {:>11} {:>10} {:>12}  {:<4} {}",
            round.round,
            round.start_tick,
            round.freeze_end,
            round.end_tick,
            round
                .officially_ended
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".to_string()),
            round.winner,
            round.win_reason,
        );
    }
    Ok(())
}

fn cmd_retarget(
    path: &Path,
    map: &str,
    sky: Option<&str>,
    addon: Option<&str>,
    keep_manifests: bool,
    output: &Path,
    force: bool,
) -> Result<()> {
    if output.exists() && !force {
        bail!("{} already exists (pass --force)", output.display());
    }
    if output.exists() && std::fs::canonicalize(path)? == std::fs::canonicalize(output)? {
        bail!("source and output must be different files");
    }
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let options = retarget::RetargetOptions {
        map,
        sky,
        addon,
        keep_manifests,
    };
    let report = retarget::retarget(&demo, &index, &options, &temporary)?;
    println!("source              {}", path.display());
    println!("target map          {map}");
    println!("sky spawn group     {}", sky.unwrap_or("dropped"));
    println!("workshop addon      {}", addon.unwrap_or("none"));
    println!(
        "manifests           {}",
        if keep_manifests { "kept" } else { "cleared" }
    );
    println!("FileHeader patched  {}", report.file_header_patched);
    println!("svc_ServerInfo      {} patched", report.server_info_patched);
    println!(
        "spawn groups        {} patched, {} dropped",
        report.spawn_groups_patched, report.spawn_groups_dropped
    );
    println!(
        "signon states       {} patched",
        report.signon_states_patched
    );
    println!("packets rewritten   {}", report.packets_rewritten);
    println!(
        "output              {} ({})",
        output.display(),
        human(report.output_bytes)
    );

    let written = map_demo(&temporary)?;
    let out_index = DemoIndex::build(&written).context("output is not frame-aligned")?;
    println!();
    if !print_checks(&out_index) {
        bail!("structural verification failed");
    }
    drop(written);
    publish_verified(&temporary, output, force)?;
    Ok(())
}

/// Suppress one player's shots, or list what there is to suppress.
///
/// Two edits happen in one pass over the demo. The messages that draw and sound a shot are
/// dropped outright; the entity fields a shot writes are dropped from their delta, which leaves
/// the client holding whatever the field last had. Both need the same walk, because entity state
/// is cumulative and every packet must be decoded in order whether or not it is edited.
///
/// Frames that lose nothing keep their original bytes, so the difference between source and
/// output stays as small as the edit itself.
#[allow(clippy::too_many_arguments)]

/// `<a>:<b>:<c>`, each 0-255 or `-` to leave as recorded.
fn parse_aim_weights(text: &str) -> Result<[Option<u32>; 3], String> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected a:b:c, got {text}"));
    }
    let mut out = [None; 3];
    for (slot, part) in parts.iter().enumerate() {
        let part = part.trim();
        if part == "-" {
            continue;
        }
        let value: u32 = part.parse().map_err(|_| format!("bad weight {part}"))?;
        if value > 255 {
            return Err(format!("weight {value} is over 255"));
        }
        out[slot] = Some(value);
    }
    Ok(out)
}

/// `<first>:<second>`, each -1..1.
fn parse_aim_hold(text: &str) -> Result<(f32, f32), String> {
    let (a, b) = text
        .split_once(':')
        .ok_or_else(|| format!("expected first:second, got {text}"))?;
    Ok((
        a.trim().parse().map_err(|_| format!("bad setting {a}"))?,
        b.trim().parse().map_err(|_| format!("bad setting {b}"))?,
    ))
}

/// `<entity>:<slot>:<sticker id>`.
fn parse_sticker(text: &str) -> Result<(i32, u32, u32), String> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected entity:slot:id, got {text}"));
    }
    Ok((
        parts[0]
            .trim()
            .parse()
            .map_err(|_| format!("bad entity {}", parts[0]))?,
        parts[1]
            .trim()
            .parse()
            .map_err(|_| format!("bad slot {}", parts[1]))?,
        parts[2]
            .trim()
            .parse()
            .map_err(|_| format!("bad sticker {}", parts[2]))?,
    ))
}

/// `<entity>:<paint kit>`.
fn parse_paint_swap(text: &str) -> Result<(i32, f32), String> {
    let (entity, kit) = text
        .split_once(':')
        .ok_or_else(|| format!("expected entity:kit, got {text}"))?;
    Ok((
        entity
            .trim()
            .parse()
            .map_err(|_| format!("bad entity {entity}"))?,
        kit.trim().parse().map_err(|_| format!("bad kit {kit}"))?,
    ))
}

/// `from:to`, a pair of clip indices.
fn parse_clip_swap(text: &str) -> Result<(u32, u32), String> {
    let (from, to) = text
        .split_once(':')
        .ok_or_else(|| format!("expected from:to, got {text}"))?;
    Ok((
        from.trim()
            .parse()
            .map_err(|_| format!("bad clip {from}"))?,
        to.trim().parse().map_err(|_| format!("bad clip {to}"))?,
    ))
}

/// `<clip>`, `<clip>:<weight>`, or `<clip>:<weight>:<mask>`.
///
/// Weight defaults to 255 because the first thing anyone wants from this is an unmistakable
/// change; a subtle blend cannot be told apart from an append that silently did nothing.
fn parse_append_clip(text: &str) -> Result<(u32, u32, Option<u32>), String> {
    let mut parts = text.split(':');
    let clip = parts
        .next()
        .ok_or_else(|| format!("expected a clip index, got {text}"))?
        .trim();
    // `ref` appends a reference pose instead of a clip sample: no clip id to get wrong, no payload
    // cost, and at full weight it snaps the character to its bind pose, which is the least
    // ambiguous thing this can be asked to do.
    let clip: u32 = if clip.eq_ignore_ascii_case("ref") {
        u32::MAX
    } else if clip.eq_ignore_ascii_case("blend") {
        u32::MAX - 1
    } else if let Some(rest) = clip.strip_prefix("type") {
        // type<id>[.<deps>] — probe a task class that occurs in no recording.
        let (id, deps) = match rest.split_once('.') {
            Some((id, deps)) => (id, deps),
            None => (rest, "1"),
        };
        let id: u32 = id.parse().map_err(|_| format!("bad type id {id}"))?;
        let deps: u32 = deps
            .parse()
            .map_err(|_| format!("bad dependency count {deps}"))?;
        if id >= 32 {
            return Err(format!("type id {id} needs more than five bits"));
        }
        if deps > 3 {
            return Err(format!(
                "{deps} dependencies is beyond anything the format uses"
            ));
        }
        0x8000_0000 | (deps << 8) | id
    } else {
        let parsed: u32 = clip.parse().map_err(|_| format!("bad clip {clip}"))?;
        if parsed >= 1024 {
            return Err(format!("clip {parsed} does not fit in the ten bit index"));
        }
        parsed
    };

    let weight = match parts.next() {
        None => 255,
        Some(raw) => {
            let value: u32 = raw
                .trim()
                .parse()
                .map_err(|_| format!("bad weight {raw}"))?;
            if value > 255 {
                return Err(format!("weight {value} does not fit in eight bits"));
            }
            value
        }
    };

    let mask = match parts.next() {
        None => None,
        Some(raw) => {
            let value: u32 = raw.trim().parse().map_err(|_| format!("bad mask {raw}"))?;
            if value >= 16 {
                return Err(format!("mask {value} does not fit in four bits"));
            }
            Some(value)
        }
    };

    if parts.next().is_some() {
        return Err(format!("expected clip[:weight[:mask]], got {text}"));
    }
    Ok((clip, weight, mask))
}

fn cmd_suppress(
    path: &Path,
    list: bool,
    entity: Option<i32>,
    from: Option<i32>,
    to: Option<i32>,
    keep_sound: bool,
    effects_only: bool,
    aim: Option<(f32, f32)>,
    aim_mode: Option<u32>,
    aim_offset: Option<(f32, f32)>,
    aim_weights: Option<[Option<u32>; 3]>,
    aim_sweep: Option<u8>,
    add_sticker: &[(i32, u32, u32)],
    swap_paint: &[(i32, f32)],
    spin: Option<f32>,
    spin_pitch: Option<f32>,
    swap_clip: &[(u32, u32)],
    append_clip: Option<(u32, u32, Option<u32>)>,
    skip_weapon: &[i32],
    strip_usercmds: bool,
    allow_orphan_deaths: bool,
    allow_orphan_damage: bool,
    output: Option<&Path>,
    force: bool,
) -> Result<()> {
    use csgoproto::{CDemoFullPacket, CDemoPacket, CDemoStringTables};
    use prost::Message as _;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{Seek, Write};

    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let shots = suppress::shots(&demo, &index)?;
    let from_tick = from.unwrap_or(i32::MIN);
    let to_tick = to.unwrap_or(i32::MAX);

    if list {
        println!("file {}", path.display());
        println!("{} discharges\n", shots.len());
        let mut by_entity: BTreeMap<i32, Vec<&suppress::Shot>> = BTreeMap::new();
        for shot in &shots {
            by_entity.entry(shot.entity_index).or_default().push(shot);
        }
        println!("  entity  handle      shots  first..last tick");
        for (entity_index, fired) in &by_entity {
            println!(
                "  {:>6}  {:<10}  {:>5}  {}..{}",
                entity_index,
                fired[0].player_handle,
                fired.len(),
                fired.iter().map(|s| s.tick).min().unwrap_or(0),
                fired.iter().map(|s| s.tick).max().unwrap_or(0),
            );
        }
        if let Some(wanted) = entity {
            let selected = shots
                .iter()
                .filter(|s| s.entity_index == wanted && s.tick >= from_tick && s.tick <= to_tick)
                .collect::<Vec<_>>();
            println!(
                "\nentity {wanted} in ticks {from_tick}..{to_tick}: {} shots",
                selected.len()
            );
            for shot in selected.iter().take(40) {
                println!(
                    "  tick {:>7}  frame {:>7}  weapon {}",
                    shot.tick, shot.frame, shot.weapon_id
                );
            }
        }
        return Ok(());
    }

    let entity_index =
        entity.context("--entity is required; run with --list to see the shooters")?;
    let output = output.context("--output is required when writing an edited demo")?;
    if output.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            output.display()
        );
    }

    let targeted = shots
        .iter()
        .filter(|s| s.entity_index == entity_index && s.tick >= from_tick && s.tick <= to_tick)
        .count();
    if targeted == 0 {
        bail!("entity {entity_index} fired no shots in ticks {from_tick}..{to_tick}");
    }

    // A shot that connected leaves damage the edit cannot explain, and a shot that killed leaves
    // damage the edit cannot even remove. Both are checked before anything is written.
    let shot_ticks = shots
        .iter()
        .filter(|s| s.entity_index == entity_index && s.tick >= from_tick && s.tick <= to_tick)
        .map(|s| s.tick)
        .collect::<Vec<_>>();
    let orphans = suppress::orphaned_damage(&demo, &index, entity_index, &shot_ticks, 4)?;
    if !orphans.is_empty() {
        println!(
            "{} damage event(s) would be left unexplained:",
            orphans.len()
        );
        for orphan in &orphans {
            println!(
                "  tick {:>6}  {:<13} victim pawn {:<4} {:>3} damage  {}",
                orphan.tick, orphan.event, orphan.victim_pawn, orphan.damage, orphan.weapon
            );
        }
        println!();
    }
    // Both refuse by default, because both leave the demo saying two different things. They are
    // separate flags because only one of them is a limitation rather than a choice: a wound could
    // in principle be edited out by holding the victim's health, while a death could not.
    let deaths = orphans.iter().filter(|o| o.fatal).count();
    if deaths > 0 && !allow_orphan_deaths {
        bail!(
            "{deaths} of the suppressed shots killed someone, and a death cannot be undone: the recording stops simulating a dead player, so there is no track to put them back on. Narrow the tick range to shots that did not connect, or pass --allow-orphan-deaths to write a demo in which {deaths} player(s) fall over unshot."
        );
    }
    let wounds = orphans.iter().filter(|o| !o.fatal).count();
    if wounds > 0 && !allow_orphan_damage {
        bail!(
            "{wounds} of the suppressed shots wounded someone. Their health still drops on those ticks, with nothing on screen to explain it. Narrow the tick range to shots that did not connect, or pass --allow-orphan-damage to write it anyway."
        );
    }

    // Each discharge names the weapon it came from, and the muzzle flash is a particle hung off
    // that weapon rather than off the player, so the weapon indices are needed to remove it.
    let weapon_entities = shots
        .iter()
        .filter(|s| s.entity_index == entity_index && s.tick >= from_tick && s.tick <= to_tick)
        .map(|s| suppress::entity_index(s.weapon_id))
        .collect::<BTreeSet<_>>();
    let suppression = suppress::Suppression {
        entity_index,
        from_tick,
        to_tick,
        drop_weapon_sound: !keep_sound,
        weapon_entities,
        dropped_particles: Default::default(),
        // The same records that say a discharge registered on another entity also say which one
        // and when, which is all the impact effects need to be found.
        impact_entities: orphans
            .iter()
            .filter(|o| o.victim_pawn > 0)
            .map(|o| (o.victim_pawn, o.tick))
            .collect(),
        impact_window: 4,
        shot_ticks: shot_ticks.iter().copied().collect(),
        // On by default: a suppression that leaves blood up the wall and a headshot spark from a
        // shot the demo no longer contains has not removed the shot, only the gun.
        drop_impact_effects: true,
        hurt_event_ids: suppress::event_ids(&demo, &index, &["player_hurt"])?,
        dropped_sounds: Default::default(),
        // On. The earlier measurement that this changed nothing a viewer could see was taken with
        // TrueView off, which is the one case where it is the only thing that matters. CS2's
        // TrueView re-predicts a demo's first person view from the recorded player's user
        // commands, so with the attack button still pressed the client simulates the whole shot
        // locally: muzzle flash, weapon animation, and the magazine counting down. A capture of a
        // demo holding zero discharges, with every firing field stripped from the weapon entity,
        // still showed a flash and the ammo falling 30 to 29 — a number that cannot have come
        // from the file. No amount of entity editing reaches that; only the input does.
        //
        // It does cost file size, because an edited command loses its delta encoding and is
        // written back in full. Only the edited player's stream grows. See ANIMTASK_FORMAT.md.
        clear_attack_input: true,
        // Every player, because a trigger pull that produced no discharge is still a trigger pull
        // the client will happily simulate.
        clear_all_attack_input: true,
        usercmd_stats: Default::default(),
        usercmd_baselines: Default::default(),
    };
    let mut stats = suppress::SuppressStats::default();
    let mut stripped_commands = 0usize;

    // The entity editor needs the demo's serializers, which means a first pass for the schema.
    let builder = boundary::BoundaryBuilder::new(&demo, &index)?;
    let mut targets: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    if !effects_only {
        targets.insert(
            entity_index,
            boundary::SHOT_PAWN_FIELDS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        );
        // A skipped weapon must also be stopped from announcing its own deploy, or it is drawn
        // regardless of what the pawn is holding.
        for weapon in skip_weapon {
            targets.entry(*weapon).or_insert_with(|| {
                boundary::WEAPON_DEPLOY_FIELDS
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect()
            });
        }
        // A suppressed shot that wounded someone leaves the wound behind, and the client draws
        // the blood and the spark itself from the reaction fields: which bone was struck, and the
        // flinch. Health and the damage event are left alone — the round's outcome is not ours to
        // change — but nothing needs to visibly recoil from a shot the demo no longer contains.
        for victim in orphans
            .iter()
            .filter(|o| o.victim_pawn > 0)
            .map(|o| o.victim_pawn)
            .collect::<BTreeSet<_>>()
        {
            targets.entry(victim).or_insert_with(|| {
                boundary::HIT_REACTION_FIELDS
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect()
            });
        }
        // Each discharge names the weapon entity it came from, so the magazine and fire timing
        // are reachable without tracking the pawn's active-weapon handle separately.
        for shot in shots
            .iter()
            .filter(|s| s.entity_index == entity_index && s.tick >= from_tick && s.tick <= to_tick)
        {
            targets
                .entry(suppress::entity_index(shot.weapon_id))
                .or_insert_with(|| {
                    boundary::SHOT_WEAPON_FIELDS
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect()
                });
        }
    }
    let weapon_targets = targets.len().saturating_sub(1);
    // Withholding the animation payload, even for a single tick, crashed CS2 during playback.
    // Byte zero of that payload is a per-tick counter, so the client evidently requires an
    // unbroken sequence and cannot be handed a gap. The mechanism is kept because it is useful
    // for fields that tolerate it; the payload itself is left alone. See POSE_DECODE_PLAN.md.
    // The animation payload is never withheld. Holding all of it for one tick crashed CS2, and so
    // did holding all but its sequence counter, so the client validates more than the sequence.
    // The mechanism stays for fields that tolerate it; this payload is not one. See
    // POSE_DECODE_PLAN.md sections 9 and 12.
    let pulse_targets: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    let mut editor = boundary::EntityEditor::new(
        &builder,
        boundary::EntityEdit {
            targets,
            model_remap: Default::default(),
            pawn_mesh_group_remaps: Default::default(),
            pose_seed: None,
            default_controller_seed: None,
            weapon_timing_seed: None,
            hud_weapon_state_seed: None,
            scheduled_writes: Default::default(),
            audit_entity_fields: Default::default(),
            allow_scheduled_create_writes: false,
            create_only_scheduled_writes: false,
            from_tick,
            to_tick,
            pulse_targets,
            pulse_ticks: shot_ticks.iter().copied().collect(),
            // The firing animation lives in the pose payload, not in any field that can be
            // dropped, so the shooter's attack overlay is held still instead.
            freeze_overlay: if effects_only {
                Default::default()
            } else {
                std::iter::once(entity_index).collect()
            },
            // Which clip carries this player's firing animation. Identified from the unedited
            // demo, because the edit is what removes the evidence of which one it is.
            skip_weapons: skip_weapon.iter().copied().collect(),
            swap_paint: swap_paint.iter().copied().collect(),
            add_stickers: {
                let mut out: BTreeMap<i32, Vec<(u32, u32)>> = BTreeMap::new();
                for (entity, slot, sticker) in add_sticker {
                    out.entry(*entity).or_default().push((*slot, *sticker));
                }
                out
            },
            // Degrees per second, at sixty four ticks to the second.
            aim_hold: aim.map(|(a, b)| (entity_index, a, b)),
            aim_offset: aim_offset.map(|(a, b)| (entity_index, a, b)),
            aim_weights: aim_weights.map(|w| (entity_index, w)),
            aim_mode: aim_mode.map(|mode| (entity_index, mode)),
            aim_sweep: aim_sweep.map(|which| (entity_index, which)),
            spin: spin.map(|rate| (entity_index, rate / 64.0)),
            // Level unless asked otherwise; the angle is now written exactly, so there is no
            // recorded direction to inherit a pitch from.
            spin_pitch: spin_pitch,
            swap_clips: if swap_clip.is_empty() {
                Default::default()
            } else {
                std::iter::once((entity_index, swap_clip.iter().copied().collect())).collect()
            },
            append_clip: append_clip
                .map(|spec| std::iter::once((entity_index, spec)).collect())
                .unwrap_or_default(),
            overlay_clip: if effects_only {
                Default::default()
            } else {
                suppress::attack_overlay_clips(&demo, &index, &builder)?
                    .into_iter()
                    .filter(|(entity, _)| *entity == entity_index)
                    .collect()
            },
        },
    );

    let temp = output.with_extension("partial");
    if let Some(parent) = temp.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(&temp)?);
    out.write_all(&demo[..16])?;

    // A rewritten frame is a different length from the one it replaces, so every frame after it
    // moves. The two pointers in the 16-byte short header address DEM_FileInfo and the post-stop
    // DEM_SpawnGroups by absolute offset, so they are recorded as the file is written and patched
    // afterwards rather than copied from the source.
    let mut offset: u64 = 16;
    let mut file_info_offset: Option<u64> = None;
    let mut spawn_groups_offset: Option<u64> = None;
    let mut seen_stop = false;

    for frame in &index.frames {
        match frame.cmd {
            CMD_FILE_INFO => file_info_offset = Some(offset),
            CMD_STOP => seen_stop = true,
            CMD_SPAWN_GROUPS if seen_stop => spawn_groups_offset = Some(offset),
            _ => {}
        }

        let tick = frame.tick();
        let replacement: Option<Vec<u8>> = match frame.cmd {
            CMD_STRING_TABLES => {
                editor.string_tables(&CDemoStringTables::decode(
                    suppress::frame_payload(&demo, frame)?.as_slice(),
                )?);
                None
            }
            CMD_FULL_PACKET => {
                editor.note_checkpoint(tick);
                let mut full =
                    CDemoFullPacket::decode(suppress::frame_payload(&demo, frame)?.as_slice())?;
                if let Some(snapshot) = &full.string_table {
                    editor.string_tables(snapshot);
                }
                match &mut full.packet {
                    // A checkpoint carries absolute state, so an edit that skipped it would be
                    // undone the moment playback reached one.
                    Some(packet) => {
                        let rebuilt = rewrite_packet_data(
                            strip_usercmds,
                            &mut stripped_commands,
                            packet.data(),
                            tick,
                            &suppression,
                            &mut editor,
                            &mut stats,
                        )?;
                        match rebuilt {
                            None => None,
                            Some(data) => {
                                packet.data = Some(data.into());
                                let mut buffer = Vec::with_capacity(full.encoded_len());
                                full.encode(&mut buffer)?;
                                Some(buffer)
                            }
                        }
                    }
                    None => None,
                }
            }
            CMD_PACKET | CMD_SIGNON_PACKET => {
                let mut packet =
                    CDemoPacket::decode(suppress::frame_payload(&demo, frame)?.as_slice())?;
                match rewrite_packet_data(
                    strip_usercmds,
                    &mut stripped_commands,
                    packet.data(),
                    tick,
                    &suppression,
                    &mut editor,
                    &mut stats,
                )? {
                    None => None,
                    Some(data) => {
                        packet.data = Some(data.into());
                        let mut buffer = Vec::with_capacity(packet.encoded_len());
                        packet.encode(&mut buffer)?;
                        Some(buffer)
                    }
                }
            }
            _ => None,
        };

        match replacement {
            None => {
                let bytes = frame.bytes(&demo);
                out.write_all(bytes)?;
                offset += bytes.len() as u64;
            }
            Some(payload) => {
                // The rewritten payload is uncompressed, so the frame's compression bit is
                // cleared rather than the payload re-compressed. Both forms are legal per frame.
                let header = frame_header(frame.cmd, false, frame.tick_raw, payload.len() as u32);
                out.write_all(&header)?;
                out.write_all(&payload)?;
                offset += header.len() as u64 + payload.len() as u64;
                stats.frames_rewritten += 1;
            }
        }
    }

    if offset > u32::MAX as u64 {
        bail!("output would be {offset} bytes, past the u32 range the short-header pointers use");
    }
    out.flush()?;
    let mut file = out
        .into_inner()
        .context("could not flush the edited demo")?;
    file.seek(std::io::SeekFrom::Start(8))?;
    file.write_all(&(file_info_offset.unwrap_or(0) as u32).to_le_bytes())?;
    file.write_all(&(spawn_groups_offset.unwrap_or(0) as u32).to_le_bytes())?;
    file.flush()?;
    drop(file);
    std::fs::rename(&temp, output)?;

    println!("source {}", path.display());
    println!("output {}", output.display());
    println!("  targeted shots       {targeted}");
    println!("  fire-bullets gone    {}", stats.fire_bullets);
    println!("  weapon sounds gone   {}", stats.weapon_sounds);
    println!(
        "  impact effects gone  {} particle(s), {} decal(s)",
        stats.impact_particles, stats.impact_decals
    );
    println!("  entity fields gone   {}", editor.stats.fields_dropped);
    println!("  weapon entities      {weapon_targets}");
    println!("  entity packets edited {}", editor.stats.packets_rewritten);
    if editor.stats.aim_written > 0 {
        println!(
            "  aim values           {} written",
            editor.stats.aim_written
        );
    }
    if editor.stats.paints_swapped > 0 {
        println!(
            "  paint kits           {} rewritten",
            editor.stats.paints_swapped
        );
    }
    if editor.stats.append_written + editor.stats.append_no_room > 0 {
        println!(
            "  appended clip        {} ticks written ({} field writes emitted), {} skipped for space, {} skipped on another slot; deepest payload {} bits of {}",
            editor.stats.append_written,
            editor.stats.append_paths_emitted,
            editor.stats.append_no_room,
            editor.stats.append_wrong_slot,
            editor.stats.append_worst_end,
            boundary::PAYLOAD_BITS
        );
    }
    if editor.stats.byte_encoding_varint + editor.stats.byte_encoding_other > 0 {
        // Evidence for whether a payload byte value can be synthesised rather than copied from a
        // value the demo happened to carry. Anything other than zero in the second column means it
        // cannot, and an appended task would have to restrict itself to observed values.
        println!(
            "  payload byte encoding {} varint, {} other",
            editor.stats.byte_encoding_varint, editor.stats.byte_encoding_other
        );
    }
    if editor.stats.stickers_added > 0 {
        println!(
            "  stickers             {} added",
            editor.stats.stickers_added
        );
    }
    if editor.stats.angles_rewritten > 0 {
        println!(
            "  view directions      {} rewritten of {} seen, {} injected",
            editor.stats.angles_rewritten, editor.stats.angles_seen, editor.stats.angles_injected
        );
    }
    if editor.stats.clips_swapped > 0 || editor.stats.clips_missed > 0 {
        println!(
            "  pose clips swapped   {}, {} unreachable",
            editor.stats.clips_swapped, editor.stats.clips_missed
        );
    }
    if strip_usercmds {
        println!("  user commands        {stripped_commands} message(s) removed entirely");
    }
    println!(
        "  attack overlay       {} restart(s) held, {} unreachable",
        editor.stats.overlays_frozen, editor.stats.overlays_missed
    );
    println!("  frames rewritten     {}", stats.frames_rewritten);
    {
        let u = suppression.usercmd_stats.borrow();
        println!(
            "  user commands        {} seen, {} held-attack cleared, {} subtick presses cleared",
            u.commands_seen, u.held_cleared, u.presses_cleared
        );
    }
    println!(
        "  encoder self-check   {} entity packets re-read, {} mismatched",
        editor.stats.identity_checks, editor.stats.identity_failures
    );
    if editor.stats.identity_failures > 0 {
        bail!(
            "the entity encoder did not reproduce what it was given; the output is not trustworthy"
        );
    }
    if effects_only {
        println!("\nEntity state was left alone (--effects-only): the shots-fired counter, the");
        println!("recoil punch and the magazine still change on these ticks.");
    } else {
        println!(
            "\nThe animation pose itself is still transmitted: m_SerializePoseRecipeAG2Dynamic"
        );
        println!(
            "and m_networkAnimTiming are opaque serialized animation data and are not edited."
        );
    }
    Ok(())
}

/// Rebuild one packet's message stream, dropping the shot's messages and editing its entity data.
///
/// Returns `None` when nothing changed, so an untouched frame keeps its original bytes. The entity
/// editor is driven for every packet either way: its state is cumulative, and a packet skipped
/// because it needed no edit would still have left the state wrong for every packet after it.
fn rewrite_packet_data(
    strip_usercmds: bool,
    stripped_commands: &mut usize,
    data: &[u8],
    tick: i32,
    suppression: &suppress::Suppression,
    editor: &mut boundary::EntityEditor,
    stats: &mut suppress::SuppressStats,
) -> Result<Option<Vec<u8>>> {
    let messages = bitwriter::read_messages(data)?;
    let last_entity_message = messages
        .iter()
        .rposition(|message| message.msg_type == suppress::SVC_PACKET_ENTITIES);
    let mut kept = Vec::with_capacity(messages.len());
    let mut changed = false;

    for (message_index, message) in messages.into_iter().enumerate() {
        if suppression.drops(&message, tick) {
            match message.msg_type {
                suppress::GE_FIRE_BULLETS => stats.fire_bullets += 1,
                suppress::CS_UM_WEAPON_SOUND => stats.weapon_sounds += 1,
                suppress::UM_PARTICLE_MANAGER => stats.impact_particles += 1,
                suppress::GE_PLACE_DECAL_EVENT => stats.impact_decals += 1,
                _ => {}
            }
            changed = true;
            continue;
        }
        if message.msg_type == suppress::SVC_PACKET_ENTITIES {
            if let Some(payload) = editor.packet_entities_with_packet_end(
                &message,
                tick,
                Some(message_index) == last_entity_message,
            )? {
                changed = true;
                kept.push(bitwriter::NetMessage {
                    msg_type: message.msg_type,
                    payload,
                });
                continue;
            }
        }
        // Commands are rewritten rather than dropped: they are a per-tick stream and removing one
        // would leave a hole, which is the shape of edit this format has already refused once.
        if message.msg_type == suppress::SVC_USER_CMDS {
            // Removing the commands outright, rather than clearing the attack inside them, is the
            // only thing that stops a viewer forcing TrueView back on: it re-simulates from these,
            // so with none present there is nothing for it to rebuild a shot out of.
            if strip_usercmds {
                changed = true;
                *stripped_commands += 1;
                continue;
            }
            if let Some(payload) = suppression.rewrite_usercmds(&message, tick)? {
                changed = true;
                kept.push(bitwriter::NetMessage {
                    msg_type: message.msg_type,
                    payload,
                });
                continue;
            }
        }
        kept.push(message);
    }

    Ok(changed.then(|| bitwriter::write_messages(&kept)))
}

/// Dump the recipe payload per tick as CSV, with the labels the correlation pass needs.
///
/// Labels come from the demo rather than from the payload: the shot ticks are the ones the
/// fire-bullets temp entity names, and speed is differenced from the transmitted origin. Nothing
/// here interprets the recipe; that is the point, since its layout is what is being tested.
/// Measure whether a decal can be attributed to a shot by geometry alone.
///
/// A decal names no shooter, so the only evidence is that it lies on the line a bullet travelled,
/// shortly after it was fired. This prints the distribution of those distances so the threshold is
/// chosen from the data rather than guessed — and so the cases it cannot separate are visible.
fn cmd_decals(path: &Path, entity: i32, window: i32) -> Result<()> {
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let shots = suppress::shots(&demo, &index)?;
    let decals = suppress::decals(&demo, &index)?;
    println!("{} decals, {} shots", decals.len(), shots.len());

    let mine = shots.iter().filter(|s| s.entity_index == entity).count();
    let with_ray = shots
        .iter()
        .filter(|s| s.entity_index == entity && s.origin.is_some() && s.angles.is_some())
        .count();
    println!(
        "entity {entity}: {mine} shots, {with_ray} carrying a usable ray
"
    );

    // Which point the bullet actually starts from is the thing to establish first: the message
    // carries both an origin and the entity's own origin, and an eye-height error would look
    // exactly like a couple of degrees of spread at typical engagement range.
    let variants: [(&str, fn(&suppress::Shot) -> Option<[f32; 3]>); 4] = [
        ("origin", |s| s.origin),
        ("origin +64z (eye)", |s| {
            s.origin.map(|o| [o[0], o[1], o[2] + 64.0])
        }),
        ("ent_origin", |s| s.ent_origin),
        ("ent_origin +64z", |s| {
            s.ent_origin.map(|o| [o[0], o[1], o[2] + 64.0])
        }),
    ];

    for (label, pick) in variants {
        let mut scores: Vec<f32> = Vec::new();
        for decal in &decals {
            let mut best: Option<f32> = None;
            for shot in &shots {
                let (Some(origin), Some(ang)) = (pick(shot), shot.angles) else {
                    continue;
                };
                if decal.tick < shot.tick || decal.tick > shot.tick + window {
                    continue;
                }
                let Some((offset, along)) =
                    suppress::ray_offset(origin, suppress::shot_direction(ang), decal.position)
                else {
                    continue;
                };
                let degrees = (offset / along.max(1.0)).atan().to_degrees();
                if best.is_none_or(|b| degrees < b) {
                    best = Some(degrees);
                }
            }
            if let Some(b) = best {
                scores.push(b);
            }
        }
        scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if scores.is_empty() {
            println!("  {label:<20} no candidates");
            continue;
        }
        let pct = |q: usize| scores[(scores.len() - 1) * q / 100];
        let within1 = scores.iter().filter(|d| **d <= 1.0).count();
        println!(
            "  {label:<20} p10 {:>6.2}  p50 {:>6.2}  p90 {:>6.2} deg   within 1 deg: {}/{}",
            pct(10),
            pct(50),
            pct(90),
            within1,
            scores.len()
        );
    }
    Ok(())
}

fn cmd_events(path: &Path, from: i32, to: i32) -> Result<()> {
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let events = suppress::game_events(&demo, &index, &suppress::DAMAGE_EVENTS)?;
    let shown = events
        .iter()
        .filter(|e| e.tick >= from && e.tick <= to)
        .collect::<Vec<_>>();
    println!(
        "{} damage events in ticks {from}..{to} (of {} total)",
        shown.len(),
        events.len()
    );
    for event in shown {
        println!("  tick {:>6}  {}", event.tick, event.name);
        for (key, value) in &event.keys {
            println!("      {key:<18} {value}");
        }
    }
    Ok(())
}

fn cmd_pose(path: &Path, entity: i32, from: i32, to: i32, output: &Path) -> Result<()> {
    use std::io::Write;
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let samples = boundary::pose_samples(&demo, &index, entity, from, to)?;
    let shots = suppress::shots(&demo, &index)?
        .into_iter()
        .filter(|shot| shot.entity_index == entity)
        .map(|shot| shot.tick)
        .collect::<std::collections::BTreeSet<_>>();

    let width = samples
        .iter()
        .map(|s| {
            s.bytes
                .iter()
                .rposition(|b| b.is_some())
                .map_or(0, |i| i + 1)
        })
        .max()
        .unwrap_or(0);

    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    write!(out, "tick,slot,topology_hex,writes,firing,since_fire,speed,body_x,body_y,body_yaw,eye_pitch,eye_yaw,active_weapon_handle,flags,ground_handle,duck_amount,desires_duck,walking,life_state,aim_raw_a,aim_raw_b,aim_at_a,aim_at_b,samplers,aim_punch_pitch,aim_punch_yaw,aim_punch_vel_pitch,aim_punch_vel_yaw,aim_punch_tick,aim_punch_fraction,view_punch_pitch,view_punch_yaw")?;
    for i in 0..width {
        write!(out, ",b{i}")?;
    }
    write!(
        out,
        ",body_z,body_z_written,payload_len,body_yaw_provenance"
    )?;
    writeln!(out)?;

    let mut last_shot: Option<i32> = None;
    let mut previous: Option<(i32, (f32, f32, f32), bool)> = None;
    for sample in &samples {
        if shots.contains(&sample.tick) {
            last_shot = Some(sample.tick);
        }
        let since = last_shot.map_or(-1, |tick| sample.tick - tick);
        let speed = previous.and_then(|(tick, (x, y, _), valid)| {
            if valid && sample.position_valid && sample.tick == tick + 1 {
                let (dx, dy) = (sample.position.0 - x, sample.position.1 - y);
                Some((dx * dx + dy * dy).sqrt())
            } else {
                None
            }
        });
        previous = Some((sample.tick, sample.position, sample.position_valid));
        let decimal = |value: Option<f32>| value.map(|v| format!("{v:.3}")).unwrap_or_default();
        let integer = |value: Option<u32>| value.map(|v| v.to_string()).unwrap_or_default();
        let boolean =
            |value: Option<bool>| value.map(|v| u8::from(v).to_string()).unwrap_or_default();
        let aim = sample
            .topology
            .as_deref()
            .and_then(poserecipe::topology)
            .and_then(|sequence| {
                let payload = sample
                    .bytes
                    .iter()
                    .copied()
                    .take_while(Option::is_some)
                    .collect::<Option<Vec<u8>>>()?;
                let (a, b) = poserecipe::aim_fields(&sequence, &payload)?;
                Some((
                    a,
                    b,
                    poserecipe::read_u16(&payload, a)?,
                    poserecipe::read_u16(&payload, b)?,
                ))
            });
        let samplers = sample
            .topology
            .as_deref()
            .and_then(poserecipe::topology)
            .and_then(|sequence| {
                let payload = sample
                    .bytes
                    .iter()
                    .copied()
                    .take_while(Option::is_some)
                    .collect::<Option<Vec<u8>>>()?;
                poserecipe::samplers(&sequence, &payload)
            })
            .map(|items| {
                items
                    .iter()
                    .map(|item| format!("{}:{}:{}", item.clip, item.time, item.time_at))
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .unwrap_or_default();
        write!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            sample.tick,
            sample.slot.map_or(-1i64, i64::from),
            sample
                .topology
                .as_ref()
                .map(|data| data
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>())
                .unwrap_or_default(),
            sample.writes,
            u8::from(shots.contains(&sample.tick)),
            since,
            decimal(speed),
            decimal(sample.position_valid.then_some(sample.position.0)),
            decimal(sample.position_valid.then_some(sample.position.1)),
            decimal(sample.body_yaw),
            decimal(sample.eye_pitch),
            decimal(sample.eye_yaw),
            integer(sample.active_weapon_handle),
            integer(sample.flags),
            integer(sample.ground_handle),
            decimal(sample.duck_amount),
            boolean(sample.desires_duck),
            boolean(sample.walking),
            integer(sample.life_state),
            aim.map(|(_, _, value, _)| value.to_string())
                .unwrap_or_default()
        )?;
        write!(
            out,
            ",{},{},{},{}",
            aim.map(|(_, _, _, value)| value.to_string())
                .unwrap_or_default(),
            aim.map(|(at, _, _, _)| at.to_string()).unwrap_or_default(),
            aim.map(|(_, at, _, _)| at.to_string()).unwrap_or_default(),
            samplers
        )?;
        write!(
            out,
            ",{},{},{},{},{},{},{},{}",
            decimal(sample.aim_punch_angle.map(|v| v.0)),
            decimal(sample.aim_punch_angle.map(|v| v.1)),
            decimal(sample.aim_punch_velocity.map(|v| v.0)),
            decimal(sample.aim_punch_velocity.map(|v| v.1)),
            sample
                .aim_punch_tick
                .map(|v| v.to_string())
                .unwrap_or_default(),
            decimal(sample.aim_punch_fraction),
            decimal(sample.view_punch_angle.map(|v| v.0)),
            decimal(sample.view_punch_angle.map(|v| v.1))
        )?;
        for i in 0..width {
            match sample.bytes.get(i).copied().flatten() {
                Some(byte) => write!(out, ",{byte}")?,
                None => write!(out, ",")?,
            }
        }
        write!(
            out,
            ",{},{},{},{}",
            sample
                .position_z_valid
                .then(|| format!("{:.5}", sample.position.2))
                .unwrap_or_default(),
            u8::from(sample.position_z_written),
            sample
                .payload_len
                .map(|value| value.to_string())
                .unwrap_or_default(),
            sample.body_yaw_provenance.unwrap_or_default()
        )?;
        writeln!(out)?;
    }
    out.flush()?;

    println!("entity {entity}, ticks {from}..{to}");
    println!("  samples      {}", samples.len());
    println!("  payload width {width} elements");
    println!("  shot ticks   {}", shots.range(from..=to).count());
    println!("  written to   {}", output.display());
    Ok(())
}

fn weapon_entity_index(handle: u32) -> i32 {
    (handle & 0x3fff) as i32
}

#[derive(Debug, PartialEq, Eq)]
enum WeaponHandleState<'a> {
    Unobserved,
    Current { created: i32, class: &'a str },
    NotCurrent,
}

fn weapon_handle_state<'a>(
    history: Option<&'a [(i32, Option<(u32, String)>)]>,
    tick: i32,
    handle: u32,
) -> WeaponHandleState<'a> {
    let Some(history) = history else {
        return WeaponHandleState::Unobserved;
    };
    let Some(index) = history.iter().rposition(|(when, _)| *when <= tick) else {
        return WeaponHandleState::Unobserved;
    };
    let (event_tick, state) = &history[index];
    // Scans preserve ticks but not an intra-tick ordinal. A distinct delete
    // or create in this tick makes this active write's identity ambiguous.
    if *event_tick == tick
        && history
            .iter()
            .any(|(when, other)| when == event_tick && other != state)
    {
        return WeaponHandleState::NotCurrent;
    }
    match state {
        Some((wire_handle, class)) if *wire_handle == handle => {
            // Full-packet checkpoints repeat a live create; they do not
            // restart the entity lifetime or erase its earlier field writes.
            let mut created = *event_tick;
            for (prior_tick, prior_state) in history[..index].iter().rev() {
                if prior_state.as_ref() == Some(&(*wire_handle, class.clone())) {
                    created = *prior_tick;
                } else {
                    break;
                }
            }
            WeaponHandleState::Current { created, class }
        }
        _ => WeaponHandleState::NotCurrent,
    }
}

fn cmd_weapon_timeline(path: &Path, output: &Path) -> Result<()> {
    use std::collections::BTreeMap;
    use std::io::Write;
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let writes = boundary::field_writes_matching(
        &demo,
        &index,
        -1,
        i32::MIN,
        i32::MAX,
        &[
            "m_hActiveWeapon",
            "m_hOuter",
            "m_nSubclassID",
            "m_iItemDefinitionIndex",
        ],
    )?;
    // CEntityHandle packs a 14-bit entity index below the 17-bit wire serial.
    // A matching create event proves the full handle even when m_hOuter is
    // absent from this recording's weapon property updates.
    let mut lifecycle: BTreeMap<i32, Vec<(i32, Option<(u32, String)>)>> = BTreeMap::new();
    for event in boundary::entity_events(&demo, &index)? {
        if event.kind == "create" {
            anyhow::ensure!(
                (0..=0x3fff).contains(&event.entity) && event.serial < (1 << 17),
                "entity create cannot form a 14+17-bit handle: entity {} serial {}",
                event.entity,
                event.serial
            );
            let handle = (event.serial << 14) | event.entity as u32;
            lifecycle
                .entry(event.entity)
                .or_default()
                .push((event.tick, Some((handle, event.class_name))));
        } else if event.kind == "delete" {
            lifecycle
                .entry(event.entity)
                .or_default()
                .push((event.tick, None));
        }
    }
    for history in lifecycle.values_mut() {
        history.sort_by_key(|(tick, _)| *tick);
    }
    let mut classes: BTreeMap<u32, String> = BTreeMap::new();
    let mut class_by_index: BTreeMap<i32, Vec<(i32, String)>> = BTreeMap::new();
    let mut subclass_by_index: BTreeMap<i32, Vec<(i32, u32)>> = BTreeMap::new();
    let mut item_def_by_index: BTreeMap<i32, Vec<(i32, u32)>> = BTreeMap::new();
    let mut active = Vec::new();
    for write in writes {
        let Some(handle) = write
            .value
            .strip_prefix("U32(")
            .and_then(|s| s.strip_suffix(')'))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let bare = write.name.rsplit('.').next().unwrap_or(&write.name);
        if bare == "m_hOuter" && weapon_entity_index(handle) == write.entity {
            let class = write.name.split('.').next().unwrap_or("").to_owned();
            if let Some(previous) = classes.insert(handle, class.clone()) {
                anyhow::ensure!(previous == class, "weapon handle {handle} changed class");
            }
        } else if bare == "m_nSubclassID" {
            let class = write.name.split('.').next().unwrap_or("").to_owned();
            class_by_index
                .entry(write.entity)
                .or_default()
                .push((write.tick, class));
            subclass_by_index
                .entry(write.entity)
                .or_default()
                .push((write.tick, handle));
        } else if bare == "m_iItemDefinitionIndex" {
            item_def_by_index
                .entry(write.entity)
                .or_default()
                .push((write.tick, handle));
        } else if bare == "m_hActiveWeapon" && write.name.starts_with("CCSPlayerPawn.") {
            active.push((write.tick, write.entity, handle));
        }
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    writeln!(out, "tick,pawn_entity,handle,weapon_entity,weapon_class,subclass_id,item_def_index,resolution,handle_proof")?;
    let mut unresolved = 0usize;
    for (tick, pawn, handle) in &active {
        let entity = weapon_entity_index(*handle);
        let state = weapon_handle_state(lifecycle.get(&entity).map(Vec::as_slice), *tick, *handle);
        // A reused or deleted slot must not lend its class or subclass to a
        // stale active handle. Without lifecycle data, preserve the older
        // m_hOuter and index-snapshot resolution as explicitly weaker proof.
        let handle_current = state != WeaponHandleState::NotCurrent;
        let created = match &state {
            WeaponHandleState::Current { created, .. } => Some(*created),
            _ => None,
        };
        let outer_exact = handle_current
            .then(|| classes.get(handle))
            .flatten()
            .map(String::as_str);
        let serial_exact = match &state {
            WeaponHandleState::Current { class, .. } => Some(*class),
            _ => None,
        };
        if let (Some(outer_class), Some(serial_class)) = (outer_exact, serial_exact) {
            anyhow::ensure!(
                outer_class == serial_class,
                "weapon handle {handle} has conflicting m_hOuter and entity-serial classes"
            );
        }
        let exact = outer_exact.or(serial_exact);
        let fallback = handle_current
            .then(|| class_by_index.get(&entity))
            .flatten()
            .and_then(|history| {
                history
                    .iter()
                    .rev()
                    .find(|(when, _)| *when <= *tick && created.is_none_or(|birth| *when >= birth))
                    .map(|(_, class)| class.as_str())
            });
        let class = exact.or(fallback).unwrap_or("");
        let subclass = handle_current
            .then(|| subclass_by_index.get(&entity))
            .flatten()
            .and_then(|history| {
                history
                    .iter()
                    .rev()
                    .find(|(when, _)| *when <= *tick && created.is_none_or(|birth| *when >= birth))
                    .map(|(_, value)| *value)
            });
        let item_def = handle_current
            .then(|| item_def_by_index.get(&entity))
            .flatten()
            .and_then(|history| {
                history
                    .iter()
                    .rev()
                    .find(|(when, _)| *when <= *tick && created.is_none_or(|birth| *when >= birth))
                    .map(|(_, value)| *value)
            });
        let resolution = if exact.is_some() {
            "exact_handle"
        } else if fallback.is_some() {
            "index_snapshot"
        } else {
            "unresolved"
        };
        let proof = if outer_exact.is_some() {
            "m_hOuter"
        } else if serial_exact.is_some() {
            "entity_serial"
        } else if fallback.is_some() {
            "index_snapshot"
        } else {
            "none"
        };
        if class.is_empty() && *handle != 0x00ff_ffff {
            unresolved += 1;
        }
        writeln!(
            out,
            "{tick},{pawn},{handle},{entity},{class},{},{},{resolution},{proof}",
            subclass.map(|v| v.to_string()).unwrap_or_default(),
            item_def.map(|v| v.to_string()).unwrap_or_default()
        )?;
    }
    out.flush()?;
    println!(
        "{} active-weapon writes, {} distinct weapon handles, {} unresolved writes -> {}",
        active.len(),
        classes.len(),
        unresolved,
        output.display()
    );
    Ok(())
}

fn cmd_fields(
    path: &Path,
    entity: i32,
    from: i32,
    to: i32,
    only: Option<&str>,
    raw: bool,
) -> Result<()> {
    use std::collections::{BTreeMap, BTreeSet};
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let writes = boundary::field_writes(&demo, &index, entity, from, to)?
        .into_iter()
        .filter(|w| only.is_none_or(|needle| w.name.contains(needle)))
        .collect::<Vec<_>>();

    println!("file   {}", path.display());
    println!("entity {entity}, ticks {from}..{to}");
    println!(
        "{} field writes
",
        writes.len()
    );

    let mut by_tick: BTreeMap<i32, Vec<&boundary::FieldWrite>> = BTreeMap::new();
    for write in &writes {
        by_tick.entry(write.tick).or_default().push(write);
    }
    for (tick, fields) in &by_tick {
        println!("  tick {tick}");
        for field in fields {
            println!(
                "    {}{:<52} {:<16} path {:?}{}",
                if entity < 0 {
                    format!("entity {:>4} ", field.entity)
                } else {
                    String::new()
                },
                field.name,
                field.value,
                field.path,
                if raw {
                    format!(" bits {}", field.bits.wire_hex())
                } else {
                    String::new()
                }
            );
        }
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for write in &writes {
        *counts.entry(write.name.as_str()).or_default() += 1;
    }
    println!(
        "
  writes per field"
    );
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (name, count) in ranked {
        println!("    {count:>5}  {name}");
    }
    Ok(())
}

/// Where a player's attack overlay restarts, against the ticks they actually fired.
fn cmd_overlay(
    path: &Path,
    entity: i32,
    shots_from: Option<&Path>,
    from: i32,
    to: i32,
) -> Result<()> {
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;

    // Shots come from the source when one is given, because an edited demo has none of its own.
    let (shot_demo, shot_index) = match shots_from {
        Some(source) => {
            let mapped = map_demo(source)?;
            let built = DemoIndex::build(&mapped)?;
            (mapped, built)
        }
        None => (map_demo(path)?, DemoIndex::build(&demo)?),
    };
    let firing: std::collections::BTreeSet<i32> = suppress::shots(&shot_demo, &shot_index)?
        .into_iter()
        .filter(|shot| shot.entity_index == entity)
        .map(|shot| shot.tick)
        .collect();

    let writes = boundary::field_writes(&demo, &index, entity, from, to)?;
    let mut tracker = poserecipe::PoseTracker::default();
    let mut tick = i32::MIN;
    let mut restarts: Vec<(i32, poserecipe::Restart)> = Vec::new();
    let mut decoded_ticks = 0usize;

    for write in &writes {
        if write.tick != tick {
            if tick != i32::MIN {
                if tracker.samplers().is_some() {
                    decoded_ticks += 1;
                }
                for restart in tracker.step() {
                    restarts.push((tick, restart));
                }
            }
            tick = write.tick;
        }
        let bare = write.name.rsplit('.').next().unwrap_or(write.name.as_str());
        let number = write
            .value
            .split_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .and_then(|inner| inner.parse::<f64>().ok());
        match bare {
            "m_topology" => {
                if let (Some(&slot), Some(raw)) =
                    (write.path.get(2), boundary::binary_block_of(&write.value))
                {
                    tracker.set_topology(slot as u32, raw);
                }
            }
            "m_SerializePoseRecipeAG2Dynamic" => {
                if let (Some(&i), Some(value)) = (write.path.last(), number) {
                    if i >= 0 {
                        tracker.set_byte(i as usize, value as u8);
                    }
                }
            }
            "m_nSerializePoseRecipeAG2ActiveSlot" => {
                if let Some(value) = number {
                    tracker.set_active(value as u32);
                }
            }
            _ => {}
        }
    }
    if tick != i32::MIN {
        for restart in tracker.step() {
            restarts.push((tick, restart));
        }
    }

    println!("file   {}", path.display());
    println!("entity {entity}");
    println!("  shots recorded     {}", firing.len());
    anyhow::ensure!(
        !firing.is_empty(),
        "no shots to check against — pass --shots-from with the unedited demo, or this check          passes for the wrong reason"
    );
    println!("  ticks decoded      {decoded_ticks}");

    // A restart within a couple of ticks of a discharge is the attack overlay; anything else is an
    // ordinary animation looping.
    let near = |tick: i32| firing.iter().any(|shot| (tick - shot).abs() <= 2);
    let mut per_clip: std::collections::BTreeMap<u32, (usize, usize, usize)> = Default::default();
    for (tick, restart) in &restarts {
        let entry = per_clip.entry(restart.clip).or_default();
        entry.0 += 1;
        if near(*tick) {
            entry.1 += 1;
            // An overlay added to the recipe rather than rewound is the first shot of a burst.
            if restart.is_new() {
                entry.2 += 1;
            }
        }
    }
    let mut ranked: Vec<_> = per_clip.into_iter().collect();
    ranked.sort_by_key(|(_, (_, on_shot, _))| std::cmp::Reverse(*on_shot));

    println!(
        "
  clip   restarts  on a shot  of those, first-of-burst"
    );
    for (clip, (total, on_shot, fresh)) in ranked.iter().take(8) {
        println!("  {clip:>5}  {total:>8}  {on_shot:>9}  {fresh:>23}");
    }
    match ranked.first() {
        Some((clip, (_, on_shot, fresh))) if *on_shot > 0 => {
            println!(
                "
attack overlay: clip {clip}, starting {on_shot} time(s) on a shot                  ({fresh} of them the first of a burst)."
            );
        }
        _ => println!(
            "
no attack overlay starts on any recorded shot."
        ),
    }
    Ok(())
}

/// Every tick on which one player's commands still ask to fire.
fn cmd_inputs(path: &Path, entity: i32, from: i32, to: i32, button: InputButton) -> Result<()> {
    use prost::Message;
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let mut ticks: Vec<(i32, i32, i32, bool, usize)> = Vec::new();
    let mut unbased = 0usize;
    let mut by_slot: std::collections::BTreeMap<(i32, i32), usize> = Default::default();
    let mut baselines: std::collections::BTreeMap<i32, csgoproto::CsgoUserCmdPb> =
        std::collections::BTreeMap::new();

    for frame in &index.frames {
        if frame.cmd != CMD_PACKET && frame.cmd != CMD_SIGNON_PACKET && frame.cmd != CMD_FULL_PACKET
        {
            continue;
        }
        let tick = frame.tick();
        if tick < from || tick > to {
            continue;
        }
        let payload = suppress::frame_payload(&demo, frame)?;
        let messages = if frame.cmd == CMD_FULL_PACKET {
            match csgoproto::CDemoFullPacket::decode(payload.as_slice())?.packet {
                Some(packet) => bitwriter::read_messages(packet.data())?,
                None => continue,
            }
        } else {
            bitwriter::read_messages(csgoproto::CDemoPacket::decode(payload.as_slice())?.data())?
        };
        for message in messages {
            if message.msg_type != suppress::SVC_USER_CMDS {
                continue;
            }
            let Ok(msg) = csgoproto::CsvcMsgUserCommands::decode(message.payload.as_slice()) else {
                continue;
            };
            for command in &msg.commands {
                let slot = command.player_slot();
                if slot < 0 {
                    continue;
                }
                let full = command.data.as_ref().filter(|d| !d.is_empty());
                let delta = command.delta_data.as_ref().filter(|d| !d.is_empty());
                let mut next = if let Some(data) = full {
                    csgoproto::CsgoUserCmdPb::decode(data.as_ref()).ok()
                } else if delta.is_some() {
                    baselines.get(&slot).cloned()
                } else {
                    continue;
                };
                if let Some(data) = delta {
                    next = next
                        .as_ref()
                        .and_then(|base| suppress::apply_delta(base, data.as_ref()));
                }
                let Some(decoded) = next else { continue };
                baselines.insert(slot, decoded.clone());
                let Some(base) = decoded.base.as_ref() else {
                    // A command with no base at all still carries buttons nowhere this audit can
                    // see, and the suppression skips it for the same reason. Counted, not hidden.
                    unbased += 1;
                    continue;
                };
                // A negative entity means "every command", which is how a leak in the handle match
                // itself becomes visible: the suppression only edits commands whose pawn handle
                // resolves to its target, so anything it cannot attribute it also cannot clear.
                let mine = suppress::entity_index(base.pawn_entity_handle()) == entity;
                if entity >= 0 && !mine {
                    continue;
                }
                let mask = button.mask();
                let held = base
                    .buttons_pb
                    .as_ref()
                    .is_some_and(|b| b.buttonstate1() & mask != 0);
                let presses = base
                    .subtick_moves
                    .iter()
                    .filter(|step| step.button() & mask != 0)
                    .count();
                if held || presses > 0 {
                    ticks.push((
                        tick,
                        slot,
                        suppress::entity_index(base.pawn_entity_handle()),
                        held,
                        presses,
                    ));
                    *by_slot
                        .entry((slot, suppress::entity_index(base.pawn_entity_handle())))
                        .or_default() += 1;
                }
            }
        }
    }

    println!("file   {}", path.display());
    println!("entity {entity}");
    println!("button {} (mask {:#x})", button.name(), button.mask());
    println!("  commands with no base {unbased}");
    if ticks.is_empty() {
        println!(
            "
no {} input remains: no held button and no subtick press.",
            button.name()
        );
        return Ok(());
    }
    println!(
        "
  player slot   pawn entity   commands carrying {} input",
        button.name()
    );
    for ((slot, pawn), count) in &by_slot {
        println!("  {slot:>11}   {pawn:>11}   {count}");
    }
    println!(
        "
{} command(s) carry {} input
",
        ticks.len(),
        button.name()
    );
    println!("  tick      slot  pawn  held  subtick presses");
    for (tick, slot, pawn, held, presses) in &ticks {
        println!(
            "  {tick:<9} {slot:<5} {pawn:<5} {:<5} {presses}",
            if *held { "yes" } else { "no" }
        );
    }
    Ok(())
}

fn cmd_roundtrip(path: &Path, limit: usize, include_full_packets: bool) -> Result<()> {
    use prost::Message;
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let mut checked = 0usize;
    let mut identical = 0usize;
    let mut mismatched: Vec<(usize, usize, usize)> = Vec::new();
    let mut messages_seen = 0usize;

    for frame in &index.frames {
        if checked >= limit {
            break;
        }
        if frame.cmd != CMD_SIGNON_PACKET
            && frame.cmd != CMD_PACKET
            && !(include_full_packets && frame.cmd == CMD_FULL_PACKET)
        {
            continue;
        }
        let raw = frame.payload(&demo);
        let decoded = if frame.compressed {
            match snap::raw::Decoder::new().decompress_vec(raw) {
                Ok(b) => b,
                Err(_) => continue,
            }
        } else {
            raw.to_vec()
        };
        let data = if frame.cmd == CMD_FULL_PACKET {
            let full = csgoproto::CDemoFullPacket::decode(&decoded[..])?;
            let packet = full
                .packet
                .context("full packet checkpoint has no nested packet")?;
            packet
                .data
                .context("full packet checkpoint has no nested packet data")?
        } else {
            let packet = match csgoproto::CDemoPacket::decode(&decoded[..]) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let Some(data) = packet.data else { continue };
            data
        };
        let messages = bitwriter::read_messages(&data)?;
        messages_seen += messages.len();
        let (reencoded, bits) = bitwriter::write_messages_with_bits(&messages);
        checked += 1;
        if bitwriter::streams_match(&data, &reencoded, bits) {
            identical += 1;
        } else if mismatched.len() < 5 {
            if mismatched.is_empty() {
                let first_diff = data
                    .iter()
                    .zip(&reencoded)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                println!("first mismatching packet: frame {}", frame.index);
                println!(
                    "  messages: {:?}",
                    messages
                        .iter()
                        .map(|m| (m.msg_type, m.payload.len()))
                        .take(8)
                        .collect::<Vec<_>>()
                );
                println!("  first differing byte at {first_diff}");
                let lo = first_diff.saturating_sub(4);
                let hi = (first_diff + 8).min(data.len());
                println!("  original  {:02x?}", &data[lo..hi.min(data.len())]);
                println!(
                    "  re-encoded {:02x?}",
                    &reencoded[lo..hi.min(reencoded.len())]
                );
            }
            mismatched.push((frame.index, data.len(), reencoded.len()));
        }
    }

    println!("packets checked   {checked}");
    println!("messages decoded  {messages_seen}");
    println!("byte-identical    {identical}");
    if identical == checked {
        println!("\nbit writer round-trips every packet exactly");
    } else {
        println!("mismatches (frame, original len, re-encoded len):");
        for (frame, a, b) in &mismatched {
            println!("  frame {frame}: {a} -> {b}");
        }
        bail!("round-trip is not exact; the bit writer cannot be trusted to edit packets");
    }
    Ok(())
}

fn cmd_serverinfo(path: &Path) -> Result<()> {
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    println!("file {}", path.display());
    match serverinfo::find(&demo, &index)? {
        Some(found) => {
            let i = &found.info;
            println!("svc_ServerInfo in frame {}", found.frame_index);
            println!("  protocol       {:?}", i.protocol);
            println!("  max_classes    {:?}", i.max_classes);
            println!("  max_clients    {:?}", i.max_clients);
            println!("  tick_interval  {:?}", i.tick_interval);
            println!("  game_dir       {:?}", i.game_dir);
            println!("  map_name       {:?}", i.map_name);
            println!("  addon_name     {:?}", i.addon_name);
            println!(
                "  manifest bytes {}",
                i.game_session_manifest
                    .as_ref()
                    .map(|m| m.len())
                    .unwrap_or(0)
            );
            match &i.game_session_config {
                Some(config) => {
                    println!("  session config:");
                    println!("    s1_mapname   {:?}", config.s1_mapname);
                    println!("    gamemode     {:?}", config.gamemode);
                    println!(
                        "    data bytes   {}",
                        config.data.as_ref().map(|d| d.len()).unwrap_or(0)
                    );
                }
                None => println!("  session config: absent"),
            }
        }
        None => println!("no svc_ServerInfo found"),
    }
    let header_frame = index
        .frames
        .iter()
        .find(|f| f.cmd == CMD_FILE_HEADER)
        .map(|f| f.index);
    if let Some(hf) = header_frame {
        use prost::Message;
        let frame = &index.frames[hf];
        if let Ok(header) = csgoproto::CDemoFileHeader::decode(frame.payload(&demo)) {
            println!("DEM_FileHeader");
            println!("  demo_version_name  {:?}", header.demo_version_name);
            println!("  demo_version_guid  {:?}", header.demo_version_guid);
            println!("  patch_version      {:?}", header.patch_version);
            println!("  build_num          {:?}", header.build_num);
            println!("  fullpackets_ver    {:?}", header.fullpackets_version);
            println!("  map_name           {:?}", header.map_name);
        }
    }
    let class_count = index
        .frames
        .iter()
        .find(|f| f.cmd == CMD_CLASS_INFO)
        .map(|f| f.payload_len);
    println!("DEM_ClassInfo payload bytes {:?}", class_count);
    Ok(())
}

fn cmd_tables(path: &Path, tick: Option<i32>, class: Option<&str>) -> Result<()> {
    use prost::Message;
    let demo = map_demo(path)?;
    let index = DemoIndex::build(&demo)?;
    let frame_index = match tick {
        Some(t) => index
            .full_packets
            .iter()
            .copied()
            .find(|i| index.frames[*i].tick() == t)
            .ok_or_else(|| anyhow::anyhow!("no full packet at tick {t}"))?,
        None => *index
            .full_packets
            .first()
            .ok_or_else(|| anyhow::anyhow!("no full packets"))?,
    };
    println!(
        "full packet at tick {} (frame {})",
        index.frames[frame_index].tick(),
        frame_index
    );
    let Some(bytes) = write::full_packet_string_tables(&demo, &index, frame_index)? else {
        println!("  no string_table submessage");
        return Ok(());
    };
    let tables = csgoproto::CDemoStringTables::decode(&bytes[..])?;
    println!("  encoded size {} bytes", bytes.len());
    for table in &tables.tables {
        println!(
            "  {:<28} {:>6} items",
            table.table_name(),
            table.items.len()
        );
        if table.table_name() == "instancebaseline" {
            if let Some(wanted) = class {
                let found = table.items.iter().any(|i| i.str() == wanted);
                println!("      class {wanted} present: {found}");
            }
            let mut keys: Vec<&str> = table.items.iter().map(|i| i.str()).collect();
            keys.sort_by_key(|k| k.parse::<i64>().unwrap_or(i64::MAX));
            println!("      keys: {}", keys.join(","));
        }
    }
    Ok(())
}

fn cmd_props(path: &Path, tick: i32, props: Option<&str>, single_threaded: bool) -> Result<()> {
    let demo = map_demo(path)?;
    let wanted: Vec<String> = match props {
        Some(list) => list.split(',').map(|s| s.trim().to_string()).collect(),
        None => props::DEFAULT_PROPS.iter().map(|s| s.to_string()).collect(),
    };
    let columns = props::sample(&demo, tick, &wanted, single_threaded)?;
    if columns.is_empty() {
        println!("no rows at tick {tick}");
        return Ok(());
    }
    let rows = columns.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
    println!("tick {tick} in {}", path.display());
    let header: Vec<&str> = columns.iter().map(|(n, _)| n.as_str()).collect();
    println!(
        "{}",
        header
            .iter()
            .map(|h| format!("{h:>14}"))
            .collect::<String>()
    );
    for row in 0..rows {
        let line: String = columns
            .iter()
            .map(|(_, values)| {
                let cell = values.get(row).cloned().unwrap_or_else(|| "-".to_string());
                format!("{cell:>14}")
            })
            .collect();
        println!("{line}");
    }
    Ok(())
}

fn parse_range(text: &str) -> Result<(i32, i32)> {
    let (a, b) = text
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("expected a range like 12-14, got '{text}'"))?;
    let start: i32 = a.trim().parse().context("range start")?;
    let end: i32 = b.trim().parse().context("range end")?;
    if end < start {
        bail!("range {text} is reversed");
    }
    Ok((start, end))
}

fn parse_round_list(text: &str) -> Result<Vec<i32>> {
    let mut rounds = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<i32>()
                .with_context(|| format!("invalid round '{part}'"))
        })
        .collect::<Result<Vec<_>>>()?;
    if rounds.iter().any(|round| *round <= 0) {
        bail!("round numbers must be positive");
    }
    rounds.sort_unstable();
    rounds.dedup();
    if rounds.is_empty() {
        bail!("--round-list must contain at least one round");
    }
    Ok(rounds)
}

fn round_tick_window(
    all: &[rounds::Round],
    number: i32,
    tail_ticks: i32,
    skip_buy_time: bool,
) -> Result<(i32, i32)> {
    let round = all
        .iter()
        .find(|round| round.round == number)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "round {number} not found; demo has rounds {}-{}",
                all.first().map(|round| round.round).unwrap_or(0),
                all.last().map(|round| round.round).unwrap_or(0)
            )
        })?;
    let next_start = round.next_start_tick.or_else(|| {
        all.iter()
            .filter(|candidate| candidate.start_tick > round.start_tick)
            .map(|candidate| candidate.start_tick)
            .min()
    });
    // A score change/round_end declares the winner; players can still move, fire and
    // trigger effects afterward. Preserve that entire interval until the next respawn.
    let end = next_start
        .map(|next| next.saturating_sub(1))
        .unwrap_or_else(|| {
            round
                .officially_ended
                .unwrap_or(round.end_tick.saturating_add(tail_ticks))
        });
    let start = if skip_buy_time {
        round.freeze_end.max(round.start_tick)
    } else {
        round.start_tick
    };
    Ok((start, end))
}

/// On-disk format identifier stored alongside verified DEM assets. Increment this only when the
/// emitted clip contract changes in a way that requires consumers to distinguish old outputs.
/// Full-round S2R export keeps `round_end + 450 - 3`, or 447 ticks, when no
/// authoritative `round_officially_ended` event is available. A canonical
/// round clip must cover that same fallback window so it can reproduce every
/// downstream S2R record without reopening the source demo.
pub const DEFAULT_TAIL_TICKS: i32 = 447;

pub const VERIFIED_TRIM_FORMAT_VERSION: i32 = 4;

#[derive(Debug, Clone)]
pub struct VerifiedRoundTrimRequest {
    pub round: i32,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct VerifiedTrimOptions {
    pub force: bool,
    pub parse_check: bool,
    pub tail_ticks: i32,
    pub skip_buy_time: bool,
}

impl Default for VerifiedTrimOptions {
    fn default() -> Self {
        Self {
            force: false,
            parse_check: true,
            tail_ticks: DEFAULT_TAIL_TICKS,
            skip_buy_time: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedRoundTrimOutcome {
    pub round: i32,
    pub destination: PathBuf,
    pub output_bytes: u64,
    pub checksum: String,
    pub checkpoint_tick: i32,
    pub logical_start_tick: i32,
    pub logical_end_tick: i32,
    pub file_info_offset: u32,
    pub spawn_groups_offset: u32,
    pub playback_ticks: i32,
    pub playback_frames: i32,
    pub playback_time: f32,
}

fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn publish_verified(temp: &Path, destination: &Path, force: bool) -> Result<()> {
    publish_verified_with(temp, destination, force, |from, to| {
        std::fs::rename(from, to)
    })
}

fn publish_verified_with<R>(
    temp: &Path,
    destination: &Path,
    force: bool,
    mut rename: R,
) -> Result<()>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let backup = destination.with_file_name(format!(
        "{}.replaced",
        destination
            .file_name()
            .map(|value| value.to_string_lossy())
            .unwrap_or_else(|| "output.dem".into())
    ));

    // Recover either crash boundary from a previous replacement. With both files present the new
    // destination was published and the backup is stale; with only the backup present, restore it.
    // The stable backup name makes that recovery possible across process restarts, but assumes
    // callers serialize publication to a destination: concurrent writers for one clip would share
    // this recovery file. DemoWriter serializes writes within a process; external callers must not
    // publish the same destination from multiple processes at once.
    if backup.exists() {
        if destination.exists() {
            std::fs::remove_file(&backup).with_context(|| {
                format!(
                    "could not remove stale replacement backup {}",
                    backup.display()
                )
            })?;
        } else {
            rename(&backup, destination).with_context(|| {
                format!(
                    "could not restore replacement backup {} to {}",
                    backup.display(),
                    destination.display()
                )
            })?;
        }
    }

    if !destination.exists() {
        return rename(temp, destination).with_context(|| {
            format!(
                "could not move {} to {}",
                temp.display(),
                destination.display()
            )
        });
    }
    if !force {
        bail!(
            "{} already exists (enable force to overwrite)",
            destination.display()
        );
    }

    // Windows rename cannot replace an existing file. Move the old clip aside first and restore
    // it if publication fails, so --force never turns a rename error into data loss.
    rename(destination, &backup).with_context(|| {
        format!(
            "could not preserve existing output {} before replacement",
            destination.display()
        )
    })?;
    if let Err(error) = rename(temp, destination) {
        let restore = rename(&backup, destination);
        bail!(
            "could not publish verified output {}: {}; restoring the previous output {}",
            destination.display(),
            error,
            if restore.is_ok() {
                "succeeded"
            } else {
                "failed"
            }
        );
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
}

fn write_plan_verified(
    demo: &[u8],
    index: &DemoIndex,
    plan: &TrimPlan,
    policies: Policies,
    round: i32,
    destination: &Path,
    force: bool,
    parse_check: bool,
) -> Result<VerifiedRoundTrimOutcome> {
    if destination.exists() && !force {
        bail!(
            "{} already exists (enable force to overwrite)",
            destination.display()
        );
    }
    let outcome = write::write_trimmed(demo, index, plan, policies, destination)?;
    let mapped = map_demo(&outcome.temp_path)?;
    let out_index = DemoIndex::build(&mapped).with_context(|| {
        format!(
            "output is not frame-aligned; partial output left at {}",
            outcome.temp_path.display()
        )
    })?;
    let failed_checks = out_index
        .structural_report()
        .into_iter()
        .filter(|check| !check.passed)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect::<Vec<_>>();
    if !failed_checks.is_empty() {
        bail!(
            "structural verification failed ({}); partial output left at {}",
            failed_checks.join("; "),
            outcome.temp_path.display()
        );
    }
    let expected_checkpoint_tick = plan.checkpoint_tick - policies.tick_shift(plan.checkpoint_tick);
    let first_full_tick = out_index
        .frames
        .iter()
        .find(|frame| frame.cmd == CMD_FULL_PACKET)
        .map(|frame| frame.tick());
    if first_full_tick != Some(expected_checkpoint_tick) {
        bail!(
            "first DEM_FullPacket is {:?}, expected checkpoint tick {}; partial output left at {}",
            first_full_tick,
            expected_checkpoint_tick,
            outcome.temp_path.display()
        );
    }
    if parse_check {
        rounds::parse_rounds(&mapped).with_context(|| {
            format!(
                "output does not reparse; partial output left at {}",
                outcome.temp_path.display()
            )
        })?;
    }
    // Hash the same read-only mapping used for verification. This includes the header offsets
    // patched by the seekable writer and avoids a second allocation or disk read.
    let checksum = fnv1a64(&mapped);
    drop(mapped);

    publish_verified(&outcome.temp_path, destination, force)?;

    Ok(VerifiedRoundTrimOutcome {
        round,
        destination: destination.to_path_buf(),
        output_bytes: outcome.output_bytes,
        checksum,
        checkpoint_tick: plan.checkpoint_tick,
        logical_start_tick: plan.requested_start_tick,
        logical_end_tick: plan.requested_end_tick,
        file_info_offset: outcome.file_info_offset,
        spawn_groups_offset: outcome.spawn_groups_offset,
        playback_ticks: outcome.playback_ticks,
        playback_frames: outcome.playback_frames,
        playback_time: outcome.playback_time,
    })
}

/// Writes, structurally verifies, reparses, hashes, and only then publishes every requested
/// round clip. The source is mapped/decompressed and parsed once for the entire batch.
pub fn trim_rounds_verified(
    source: &Path,
    requests: &[VerifiedRoundTrimRequest],
    options: VerifiedTrimOptions,
) -> Result<Vec<VerifiedRoundTrimOutcome>> {
    trim_rounds_verified_with_bytes(source, requests, options, None)
}

/// Reuse the import pipeline's immutable decompressed source. This changes only loading;
/// planning, atomic publication, hashing and reparsing checks are identical to the file API.
pub fn trim_rounds_verified_with_bytes(
    source: &Path,
    requests: &[VerifiedRoundTrimRequest],
    options: VerifiedTrimOptions,
    source_bytes: Option<&[u8]>,
) -> Result<Vec<VerifiedRoundTrimOutcome>> {
    trim_rounds_verified_with_source_data(source, requests, options, source_bytes, None)
}

/// Reuse round events from the source's collection-discovery pass. The same round resolver
/// and all clip verification still run; only the redundant source entity decode is avoided.
pub fn trim_rounds_verified_with_source_data(
    source: &Path,
    requests: &[VerifiedRoundTrimRequest],
    options: VerifiedTrimOptions,
    source_bytes: Option<&[u8]>,
    round_events: Option<&[parser::second_pass::game_events::GameEvent]>,
) -> Result<Vec<VerifiedRoundTrimOutcome>> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen_rounds = std::collections::HashSet::with_capacity(requests.len());
    let mut seen_destinations = std::collections::HashSet::with_capacity(requests.len());
    for request in requests {
        if request.round <= 0 {
            bail!("round numbers must be positive");
        }
        if !seen_rounds.insert(request.round) {
            bail!("round {} was requested more than once", request.round);
        }
        if !seen_destinations.insert(request.destination.clone()) {
            bail!(
                "destination {} was requested more than once",
                request.destination.display()
            );
        }
        if request.destination == source {
            bail!("destination is the same file as the source");
        }
        if request.destination.exists() && !options.force {
            bail!(
                "{} already exists (enable force to overwrite)",
                request.destination.display()
            );
        }
    }

    let policies = Policies {
        animation: AnimationPolicy::Keep,
        bootstrap: BootstrapPolicy::Full,
        metadata: MetadataPolicy::Absolute,
        spawn_groups: SpawnGroupsPolicy::Preserve,
        tickrate: 64.0,
        include_startup: true,
        sync_string_tables: true,
        align_startup: true,
        rebase_ticks: true,
    };
    let loaded;
    let demo = match source_bytes {
        Some(bytes) => bytes,
        None => {
            loaded = map_demo(source)?;
            &loaded
        }
    };
    let index = DemoIndex::build(demo)?;
    let all_rounds = match round_events {
        Some(events) => rounds::build_rounds(events)?,
        None => rounds::parse_rounds(demo)?,
    };
    let boundary = boundary::BoundaryBuilder::new(demo, &index)?;
    let write_request = |request: &VerifiedRoundTrimRequest| {
        let (start_tick, end_tick) = round_tick_window(
            &all_rounds,
            request.round,
            options.tail_ticks,
            options.skip_buy_time,
        )?;
        let prepared = boundary.prepare(demo, &index, start_tick, end_tick, policies)?;
        let demo = prepared.as_slice();
        let index = DemoIndex::build(demo)?;
        let plan = plan_trim(&index, start_tick, end_tick, policies)?;
        write_plan_verified(
            &demo,
            &index,
            &plan,
            policies,
            request.round,
            &request.destination,
            options.force,
            options.parse_check,
        )
    };
    // Integrated imports already run inside a bounded trim pool. Let its idle workers help
    // with a source containing many rounds; nested Rayon work reuses that same pool. The
    // standalone API keeps serial clip publication rather than creating an unbounded pool.
    if rayon::current_thread_index().is_some() {
        requests.par_iter().map(write_request).collect()
    } else {
        requests.iter().map(write_request).collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn write_batch_clip(
    boundary: &boundary::BoundaryBuilder,
    source: &Path,
    demo: &[u8],
    index: &DemoIndex,
    start_tick: i32,
    end_tick: i32,
    round: i32,
    policies: Policies,
    destination: &Path,
    json_report: Option<&Path>,
    dry_run: bool,
    force: bool,
    no_parse_check: bool,
) -> Result<()> {
    if destination == source {
        bail!("destination is the same file as the source");
    }
    if destination.exists() && !force && !dry_run {
        bail!(
            "{} already exists (pass --force to overwrite)",
            destination.display()
        );
    }

    let source_bytes = index.file_len;
    let prepared = boundary.prepare(demo, index, start_tick, end_tick, policies)?;
    let demo = prepared.as_slice();
    let prepared_index = DemoIndex::build(demo)?;
    let index = &prepared_index;
    let label = format!("round {round}");
    let plan = plan_trim(index, start_tick, end_tick, policies)?;
    let estimated = write::estimate_size(demo, index, &plan, policies)?;
    println!("source            {}", source.display());
    println!("selection         {label}");
    println!(
        "logical ticks     {} .. {}",
        plan.requested_start_tick, plan.requested_end_tick
    );
    println!(
        "estimated output  {} ({:.1}% of source)",
        human(estimated),
        estimated as f64 / source_bytes as f64 * 100.0
    );
    for warning in &plan.warnings {
        println!("warning           {warning}");
    }
    if dry_run {
        println!("dry run           nothing written");
        return Ok(());
    }

    let started = Instant::now();
    let outcome = write_plan_verified(
        demo,
        index,
        &plan,
        policies,
        round,
        destination,
        force,
        !no_parse_check,
    )?;
    println!(
        "wrote             {} in {:.1}s",
        human(outcome.output_bytes),
        started.elapsed().as_secs_f64()
    );
    println!(
        "verified          structure, checkpoint, parser, fnv1a {}",
        outcome.checksum
    );
    println!("output            {}", destination.display());

    if let Some(report_path) = json_report {
        let report = serde_json::json!({
            "source": source.display().to_string(),
            "source_bytes": source_bytes,
            "output": destination.display().to_string(),
            "output_bytes": outcome.output_bytes,
            "checksum": outcome.checksum,
            "selection": label,
            "round": round,
            "logical_start_tick": plan.requested_start_tick,
            "logical_end_tick": plan.requested_end_tick,
            "checkpoint_tick": plan.checkpoint_tick,
            "checkpoint_frame": plan.checkpoint_frame,
            "body_last_frame": plan.body_last_frame,
            "last_tick": plan.body_last_tick,
            "checkpoint_padding_ticks": plan.checkpoint_padding_ticks,
            "checkpoint_padding_bytes": plan.checkpoint_padding_bytes,
            "bootstrap_frames": plan.bootstrap_count,
            "bootstrap_bytes": plan.bootstrap_bytes,
            "body_frames": plan.selected.len() - plan.bootstrap_count,
            "body_bytes": plan.body_bytes,
            "packet_frames": plan.packet_frames,
            "animation_bytes_kept": plan.animation_bytes_kept,
            "animation_bytes_dropped": plan.animation_bytes_dropped,
            "tail_block_bytes_dropped": plan.tail_block_bytes_dropped,
            "file_info_offset": outcome.file_info_offset,
            "spawn_groups_offset": outcome.spawn_groups_offset,
            "playback_ticks": outcome.playback_ticks,
            "playback_frames": outcome.playback_frames,
            "playback_time": outcome.playback_time,
            "warnings": plan.warnings,
            "playback_validated": false,
        });
        std::fs::write(report_path, serde_json::to_string_pretty(&report)?)?;
        println!("report            {}", report_path.display());
    }
    Ok(())
}

fn cmd_trim_many(args: &TrimArgs, list: &str) -> Result<()> {
    let requested = parse_round_list(list)?;
    let output_directory = args
        .output_directory
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--output-directory is required with --round-list"))?;
    if !args.dry_run {
        std::fs::create_dir_all(output_directory)
            .with_context(|| format!("could not create {}", output_directory.display()))?;
        if let Some(reports) = &args.json_report_directory {
            std::fs::create_dir_all(reports)
                .with_context(|| format!("could not create {}", reports.display()))?;
        }
    }

    let policies = Policies {
        animation: match args.animation {
            AnimationArg::Keep => AnimationPolicy::Keep,
            AnimationArg::BodyOnly => AnimationPolicy::BodyOnly,
            AnimationArg::Drop => AnimationPolicy::Drop,
        },
        bootstrap: match args.bootstrap {
            BootstrapArg::Full => BootstrapPolicy::Full,
            BootstrapArg::Required => BootstrapPolicy::Required,
        },
        metadata: match args.metadata_policy {
            MetadataArg::Absolute => MetadataPolicy::Absolute,
            MetadataArg::Window => MetadataPolicy::Window,
        },
        spawn_groups: match args.spawn_groups {
            SpawnGroupsArg::Preserve => SpawnGroupsPolicy::Preserve,
            SpawnGroupsArg::Empty => SpawnGroupsPolicy::Empty,
            SpawnGroupsArg::Omit => SpawnGroupsPolicy::Omit,
        },
        tickrate: args.tickrate,
        include_startup: !args.no_startup,
        sync_string_tables: !args.no_string_table_sync,
        align_startup: !args.no_align_startup,
        rebase_ticks: !args.no_rebase,
    };

    let demo = map_demo(&args.demo)?;
    let demo_index = DemoIndex::build(&demo)?;
    let started = Instant::now();
    eprintln!(
        "resolving {} requested rounds (one full parse of the source demo)...",
        requested.len()
    );
    let all = rounds::parse_rounds(&demo)?;
    eprintln!(
        "  {} rounds in {:.1}s",
        all.len(),
        started.elapsed().as_secs_f64()
    );

    let boundary = boundary::BoundaryBuilder::new(&demo, &demo_index)?;
    let source_stem = source_demo_stem(&args.demo)?;

    println!("batch             {} distinct round clips", requested.len());
    for (clip_index, number) in requested.iter().copied().enumerate() {
        let (start_tick, end_tick) =
            round_tick_window(&all, number, args.tail_ticks, !args.include_buy_time)?;
        let file_stem = format!("{source_stem}_r{number}");
        let output = output_directory.join(format!("{file_stem}.dem"));
        let json_report = args
            .json_report_directory
            .as_ref()
            .map(|directory| directory.join(format!("{file_stem}.json")));
        println!(
            "\n=== clip {} of {}: round {} ===",
            clip_index + 1,
            requested.len(),
            number
        );

        write_batch_clip(
            &boundary,
            &args.demo,
            &demo,
            &demo_index,
            start_tick,
            end_tick,
            number,
            policies,
            &output,
            json_report.as_deref(),
            args.dry_run,
            args.force,
            args.no_parse_check,
        )?;
    }
    println!("\nbatch complete    {} round clips", requested.len());
    Ok(())
}

fn cmd_trim(args: &TrimArgs) -> Result<()> {
    if let Some(list) = &args.round_list {
        return cmd_trim_many(args, list);
    }
    let policies = Policies {
        animation: match args.animation {
            AnimationArg::Keep => AnimationPolicy::Keep,
            AnimationArg::BodyOnly => AnimationPolicy::BodyOnly,
            AnimationArg::Drop => AnimationPolicy::Drop,
        },
        bootstrap: match args.bootstrap {
            BootstrapArg::Full => BootstrapPolicy::Full,
            BootstrapArg::Required => BootstrapPolicy::Required,
        },
        metadata: match args.metadata_policy {
            MetadataArg::Absolute => MetadataPolicy::Absolute,
            MetadataArg::Window => MetadataPolicy::Window,
        },
        spawn_groups: match args.spawn_groups {
            SpawnGroupsArg::Preserve => SpawnGroupsPolicy::Preserve,
            SpawnGroupsArg::Empty => SpawnGroupsPolicy::Empty,
            SpawnGroupsArg::Omit => SpawnGroupsPolicy::Omit,
        },
        tickrate: args.tickrate,
        include_startup: !args.no_startup,
        sync_string_tables: !args.no_string_table_sync,
        align_startup: !args.no_align_startup,
        rebase_ticks: !args.no_rebase,
    };

    // Check the destination before doing anything expensive: resolving rounds costs a full
    // parse, and finding out afterwards that the output already exists is a waste.
    if !args.dry_run {
        let destination = args
            .output
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--output is required unless --dry-run is given"))?;
        if destination == &args.demo {
            bail!("destination is the same file as the source");
        }
        if destination.exists() && !args.force {
            bail!(
                "{} already exists (pass --force to overwrite)",
                destination.display()
            );
        }
    }

    let demo = map_demo(&args.demo)?;
    let index = DemoIndex::build(&demo)?;

    let (start_tick, end_tick, label) = if let Some(text) = &args.ticks {
        let (start, end) = parse_range(text)?;
        (start, end, format!("ticks {start}-{end}"))
    } else {
        let (first, last) = match (args.round, &args.rounds) {
            (Some(n), None) => (n, n),
            (None, Some(text)) => parse_range(text)?,
            _ => bail!("choose exactly one of --round, --rounds or --ticks"),
        };
        let started = Instant::now();
        eprintln!("resolving rounds (full parse of the source demo)...");
        let all = rounds::parse_rounds(&demo)?;
        eprintln!(
            "  {} rounds in {:.1}s",
            all.len(),
            started.elapsed().as_secs_f64()
        );
        let find = |n: i32| {
            all.iter().find(|r| r.round == n).ok_or_else(|| {
                anyhow::anyhow!(
                    "round {n} not found; demo has rounds {}-{}",
                    all.first().map(|r| r.round).unwrap_or(0),
                    all.last().map(|r| r.round).unwrap_or(0)
                )
            })
        };
        let first_round = find(first)?;
        let last_round = find(last)?;
        let (_, end) = round_tick_window(
            &all,
            last_round.round,
            args.tail_ticks,
            !args.include_buy_time,
        )?;
        (
            if args.include_buy_time {
                first_round.start_tick
            } else {
                first_round.freeze_end.max(first_round.start_tick)
            },
            end,
            if first == last {
                format!("round {first}")
            } else {
                format!("rounds {first}-{last}")
            },
        )
    };

    let source_bytes = index.file_len;
    let source_frames = index.frames.len();
    let boundary = boundary::BoundaryBuilder::new(&demo, &index)?;
    let prepared = boundary.prepare(&demo, &index, start_tick, end_tick, policies)?;
    let demo = prepared.as_slice();
    let index = DemoIndex::build(demo)?;
    let plan = plan_trim(&index, start_tick, end_tick, policies)?;
    let estimated = write::estimate_size(&demo, &index, &plan, policies)?;

    println!("source            {}", args.demo.display());
    println!(
        "                  {} ({} frames)",
        human(source_bytes),
        source_frames
    );
    println!("selection         {label}");
    println!(
        "logical ticks     {} .. {}",
        plan.requested_start_tick, plan.requested_end_tick
    );
    println!(
        "physical ticks    {} .. {}  (checkpoint frame {})",
        plan.checkpoint_tick, plan.body_last_tick, plan.checkpoint_frame
    );
    println!(
        "checkpoint pad    {} ticks ({:.1} s), {}",
        plan.checkpoint_padding_ticks,
        plan.checkpoint_padding_ticks as f32 / policies.tickrate,
        human(plan.checkpoint_padding_bytes)
    );
    println!(
        "policies          animation={} bootstrap={} metadata={} spawn-groups={}",
        policies.animation, policies.bootstrap, policies.metadata, policies.spawn_groups
    );
    println!(
        "bootstrap         {} frames, {}",
        plan.bootstrap_count,
        human(plan.bootstrap_bytes)
    );
    println!(
        "startup burst     {} frames, {}",
        plan.startup_count,
        human(plan.startup_bytes)
    );
    println!(
        "body              {} frames, {} ({} DEM_Packet in output)",
        plan.selected.len() - plan.prefix_count(),
        human(plan.body_bytes),
        plan.packet_frames
    );
    println!(
        "animation         kept {} frames / {}, dropped {} (of which {} is the tick-final block)",
        plan.animation_frames_kept,
        human(plan.animation_bytes_kept),
        human(plan.animation_bytes_dropped + plan.tail_block_bytes_dropped),
        human(plan.tail_block_bytes_dropped)
    );
    println!(
        "estimated output  {} ({:.1}% of source)",
        human(estimated),
        estimated as f64 / source_bytes as f64 * 100.0
    );
    for warning in &plan.warnings {
        println!("warning           {warning}");
    }

    if args.dry_run {
        println!("\ndry run — nothing written");
        return Ok(());
    }

    let destination = match &args.output {
        Some(path) => path.clone(),
        None => bail!("--output is required unless --dry-run is given"),
    };
    if destination == args.demo {
        bail!("destination is the same file as the source");
    }
    if destination.exists() && !args.force {
        bail!(
            "{} already exists (pass --force to overwrite)",
            destination.display()
        );
    }

    let started = Instant::now();
    let outcome = write::write_trimmed(&demo, &index, &plan, policies, &destination)?;
    println!(
        "\nwrote             {} in {:.1}s",
        human(outcome.output_bytes),
        started.elapsed().as_secs_f64()
    );
    if outcome.output_bytes != estimated {
        println!(
            "note              estimate was off by {} bytes",
            outcome.output_bytes as i64 - estimated as i64
        );
    }
    println!(
        "fileinfo          playback_ticks={} playback_frames={} playback_time={}",
        outcome.playback_ticks, outcome.playback_frames, outcome.playback_time
    );

    // Structural verification of the temp file, before it is allowed to become the output.
    let verified = {
        let temp = map_demo(&outcome.temp_path)?;
        let out_index = DemoIndex::build(&temp).context("output is not frame-aligned")?;
        println!();
        let ok = print_checks(&out_index);
        // The output's first full packet must be the checkpoint: it is what establishes
        // entity state, for CS2 and for the parser's single-threaded path alike.
        let expected_checkpoint_tick =
            plan.checkpoint_tick - policies.tick_shift(plan.checkpoint_tick);
        let first_full = out_index.frames.iter().find(|f| f.cmd == CMD_FULL_PACKET);
        let first_body = first_full.map(|f| f.tick()) == Some(expected_checkpoint_tick);
        println!(
            "  [{}] {:<52} {}",
            if first_body { "ok" } else { "FAIL" },
            "first DEM_FullPacket in the output is the checkpoint",
            match first_full {
                Some(f) => format!("tick {} (expected {})", f.tick(), expected_checkpoint_tick),
                None => "no full packet".to_string(),
            }
        );
        ok && first_body
    };
    if !verified {
        bail!(
            "structural verification failed; the partial output is left at {}",
            outcome.temp_path.display()
        );
    }

    if !args.no_parse_check {
        print!("\nreparsing output with this repository's parser... ");
        use std::io::Write as _;
        std::io::stdout().flush().ok();
        let temp = map_demo(&outcome.temp_path)?;
        match rounds::parse_rounds(&temp) {
            Ok(found) => println!("ok ({} rounds visible in the clip)", found.len()),
            Err(e) => {
                println!("FAILED");
                bail!(
                    "the output does not parse: {e}. Partial output left at {}",
                    outcome.temp_path.display()
                );
            }
        }
    }

    publish_verified(&outcome.temp_path, &destination, args.force)?;
    println!("\noutput            {}", destination.display());
    println!(
        "size              {} ({:.1}% of source)",
        human(outcome.output_bytes),
        outcome.output_bytes as f64 / source_bytes as f64 * 100.0
    );
    println!("status            structurally valid, parser-valid; CS2 playback NOT tested");

    if let Some(report_path) = &args.json_report {
        let report = serde_json::json!({
            "source": args.demo.display().to_string(),
            "source_bytes": source_bytes,
            "output": destination.display().to_string(),
            "output_bytes": outcome.output_bytes,
            "selection": label,
            "round": args.round,
            "logical_start_tick": plan.requested_start_tick,
            "logical_end_tick": plan.requested_end_tick,
            "checkpoint_tick": plan.checkpoint_tick,
            "checkpoint_frame": plan.checkpoint_frame,
            "body_last_frame": plan.body_last_frame,
            "last_tick": plan.body_last_tick,
            "checkpoint_padding_ticks": plan.checkpoint_padding_ticks,
            "checkpoint_padding_bytes": plan.checkpoint_padding_bytes,
            "bootstrap_frames": plan.bootstrap_count,
            "bootstrap_bytes": plan.bootstrap_bytes,
            "body_frames": plan.selected.len() - plan.bootstrap_count,
            "body_bytes": plan.body_bytes,
            "packet_frames": plan.packet_frames,
            "animation_bytes_kept": plan.animation_bytes_kept,
            "animation_bytes_dropped": plan.animation_bytes_dropped,
            "tail_block_bytes_dropped": plan.tail_block_bytes_dropped,
            "policy_animation": policies.animation.to_string(),
            "policy_bootstrap": policies.bootstrap.to_string(),
            "policy_metadata": policies.metadata.to_string(),
            "policy_spawn_groups": policies.spawn_groups.to_string(),
            "file_info_offset": outcome.file_info_offset,
            "spawn_groups_offset": outcome.spawn_groups_offset,
            "playback_ticks": outcome.playback_ticks,
            "playback_frames": outcome.playback_frames,
            "playback_time": outcome.playback_time,
            "warnings": plan.warnings,
            "playback_validated": false,
        });
        std::fs::write(report_path, serde_json::to_string_pretty(&report)?)?;
        println!("report            {}", report_path.display());
    }

    Ok(())
}

/// Stable user-facing clip stem. Compression is transport, and `.dem` is the source format;
/// neither belongs in `match_r15.dem`. Dots that are part of the actual match name are retained.
fn source_demo_stem(path: &Path) -> Result<String> {
    let mut name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("source demo has no usable file name"))?
        .to_string();

    for suffix in [".gz", ".zst"] {
        if name.to_ascii_lowercase().ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    if name.to_ascii_lowercase().ends_with(".dem") {
        name.truncate(name.len() - ".dem".len());
    }

    if name.is_empty() {
        bail!("source demo has no usable file stem");
    }
    Ok(name)
}

/// Runs the writer CLI from a supplied argument vector. Demoparser uses this entry point to expose
/// trimming as one of its own features while this crate retains its standalone development tool.
pub fn run_from(args: Vec<std::ffi::OsString>) -> Result<()> {
    let cli = Cli::parse_from(args);
    match &cli.command {
        Command::Inspect { demo, checkpoints } => cmd_inspect(demo, *checkpoints),
        Command::NetTicks { demo, from, to } => cmd_net_ticks(demo, *from, *to),
        Command::Rounds { demo } => cmd_rounds(demo),
        Command::Verify { demo } => cmd_verify(demo),
        Command::Props {
            demo,
            tick,
            props,
            single_threaded,
        } => cmd_props(demo, *tick, props.as_deref(), *single_threaded),
        Command::Tables { demo, tick, class } => cmd_tables(demo, *tick, class.as_deref()),
        Command::Serverinfo { demo } => cmd_serverinfo(demo),
        Command::Suppress {
            demo,
            list,
            entity,
            from,
            to,
            keep_sound,
            effects_only,
            aim,
            aim_mode,
            aim_offset,
            aim_weights,
            aim_sweep,
            add_sticker,
            swap_paint,
            spin,
            spin_pitch,
            swap_clip,
            append_clip,
            skip_weapon,
            strip_usercmds,
            allow_orphan_deaths,
            allow_orphan_damage,
            output,
            force,
        } => cmd_suppress(
            demo,
            *list,
            *entity,
            *from,
            *to,
            *keep_sound,
            *effects_only,
            *aim,
            *aim_mode,
            *aim_offset,
            *aim_weights,
            *aim_sweep,
            add_sticker,
            swap_paint,
            *spin,
            *spin_pitch,
            swap_clip,
            *append_clip,
            skip_weapon,
            *strip_usercmds,
            *allow_orphan_deaths,
            *allow_orphan_damage,
            output.as_deref(),
            *force,
        ),
        Command::Decals {
            demo,
            entity,
            window,
        } => cmd_decals(demo, *entity, *window),
        Command::Events { demo, from, to } => cmd_events(demo, *from, *to),
        Command::Inputs {
            demo,
            entity,
            from,
            to,
            button,
        } => cmd_inputs(demo, *entity, *from, *to, *button),
        Command::Overlay {
            demo,
            entity,
            shots_from,
            from,
            to,
        } => cmd_overlay(demo, *entity, shots_from.as_deref(), *from, *to),
        Command::Pose {
            demo,
            entity,
            from,
            to,
            output,
        } => cmd_pose(demo, *entity, *from, *to, output),
        Command::WeaponTimeline { demo, output } => cmd_weapon_timeline(demo, output),
        Command::Fields {
            demo,
            entity,
            from,
            to,
            only,
            raw,
        } => cmd_fields(demo, *entity, *from, *to, only.as_deref(), *raw),
        Command::ClassPaths { demo, class } => {
            let bytes = map_demo(demo)?;
            let index = DemoIndex::build(&bytes)?;
            for (name, path) in boundary::class_field_paths(&bytes, &index, class)? {
                println!("{name}\t{path:?}");
            }
            Ok(())
        }
        Command::Roundtrip {
            demo,
            limit,
            include_full_packets,
        } => cmd_roundtrip(demo, *limit, *include_full_packets),
        Command::Retarget {
            demo,
            map,
            sky,
            addon,
            keep_manifests,
            output,
            force,
        } => cmd_retarget(
            demo,
            map,
            sky.as_deref(),
            addon.as_deref(),
            *keep_manifests,
            output,
            *force,
        ),
        Command::Trim(args) => cmd_trim(args),
    }
}


#[cfg(test)]
mod weapon_timeline_tests {
    use super::*;

    #[test]
    fn full_handle_uses_all_fourteen_entity_bits() {
        let handle = (975 << 14) | 0x801;
        assert_eq!(weapon_entity_index(handle), 0x801);
        assert_eq!(weapon_entity_index(15_974_487), 87);
    }

    #[test]
    fn lifecycle_reuse_cannot_prove_a_stale_weapon_handle() {
        let old = (975 << 14) | 87;
        let new = (976 << 14) | 87;
        let history = [
            (1, Some((old, "CWeaponM4A1".to_owned()))),
            (5, None),
            (6, Some((new, "CWeaponAK47".to_owned()))),
        ];
        assert_eq!(
            weapon_handle_state(Some(&history), 4, old),
            WeaponHandleState::Current {
                created: 1,
                class: "CWeaponM4A1"
            }
        );
        assert_eq!(
            weapon_handle_state(Some(&history), 5, old),
            WeaponHandleState::NotCurrent
        );
        assert_eq!(
            weapon_handle_state(Some(&history), 7, old),
            WeaponHandleState::NotCurrent
        );
        assert_eq!(
            weapon_handle_state(Some(&history), 7, new),
            WeaponHandleState::Current {
                created: 6,
                class: "CWeaponAK47"
            }
        );
    }

    #[test]
    fn same_tick_reuse_is_ambiguous_but_repeated_checkpoint_is_not() {
        let old = (975 << 14) | 87;
        let new = (976 << 14) | 87;
        let history = [
            (1, Some((old, "CWeaponM4A1".to_owned()))),
            (1, Some((old, "CWeaponM4A1".to_owned()))),
            (6, None),
            (6, Some((new, "CWeaponAK47".to_owned()))),
        ];
        assert_eq!(
            weapon_handle_state(Some(&history), 1, old),
            WeaponHandleState::Current {
                created: 1,
                class: "CWeaponM4A1"
            }
        );
        assert_eq!(
            weapon_handle_state(Some(&history), 6, old),
            WeaponHandleState::NotCurrent
        );
        assert_eq!(
            weapon_handle_state(Some(&history), 6, new),
            WeaponHandleState::NotCurrent
        );
        assert_eq!(
            weapon_handle_state(Some(&history), 7, new),
            WeaponHandleState::Current {
                created: 6,
                class: "CWeaponAK47"
            }
        );
    }

    #[test]
    fn checkpoint_create_does_not_reset_weapon_metadata_lifetime() {
        let handle = (975 << 14) | 87;
        let history = [
            (1, Some((handle, "CWeaponM4A1".to_owned()))),
            (100, Some((handle, "CWeaponM4A1".to_owned()))),
        ];
        assert_eq!(
            weapon_handle_state(Some(&history), 101, handle),
            WeaponHandleState::Current {
                created: 1,
                class: "CWeaponM4A1"
            }
        );
    }
}

#[cfg(test)]
mod batch_trim_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "demo-writer-publish-test-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn round(
        number: i32,
        start_tick: i32,
        end_tick: i32,
        officially_ended: Option<i32>,
    ) -> rounds::Round {
        rounds::Round {
            round: number,
            start_tick,
            freeze_end: start_tick,
            end_tick,
            next_start_tick: None,
            officially_ended,
            winner: String::new(),
            win_reason: String::new(),
        }
    }

    #[test]
    fn round_list_is_sorted_and_deduplicated() {
        assert_eq!(parse_round_list("7, 3,7").unwrap(), vec![3, 7]);
    }

    #[test]
    fn round_list_rejects_non_positive_values() {
        assert!(parse_round_list("1,0,2").is_err());
    }

    #[test]
    fn round_window_never_overlaps_the_next_round() {
        let rounds = vec![round(1, 100, 190, Some(250)), round(2, 220, 320, Some(330))];

        assert_eq!(
            round_tick_window(&rounds, 1, DEFAULT_TAIL_TICKS, true).unwrap(),
            (100, 219)
        );
    }

    #[test]
    fn round_window_preserves_gameplay_after_early_official_end() {
        let rounds = vec![round(1, 100, 190, Some(200)), round(2, 900, 1000, None)];
        assert_eq!(
            round_tick_window(&rounds, 1, DEFAULT_TAIL_TICKS, true).unwrap(),
            (100, 899)
        );
    }

    #[test]
    fn incomplete_next_round_still_limits_the_clip() {
        let mut first = round(1, 100, 190, Some(200));
        first.next_start_tick = Some(300);
        assert_eq!(
            round_tick_window(&[first], 1, DEFAULT_TAIL_TICKS, true).unwrap(),
            (100, 299)
        );
    }

    #[test]
    fn buy_time_option_changes_start_without_shortening_post_win_gameplay() {
        let mut first = round(1, 100, 400, Some(410));
        first.freeze_end = 200;
        let rounds = vec![first, round(2, 600, 800, None)];
        assert!(VerifiedTrimOptions::default().skip_buy_time);
        assert_eq!(
            round_tick_window(&rounds, 1, DEFAULT_TAIL_TICKS, true).unwrap(),
            (200, 599)
        );
        assert_eq!(
            round_tick_window(&rounds, 1, DEFAULT_TAIL_TICKS, false).unwrap(),
            (100, 599)
        );
    }

    #[test]
    fn default_round_tail_matches_full_round_s2r_window() {
        let rounds = vec![round(1, 100, 190, None)];

        assert_eq!(DEFAULT_TAIL_TICKS, 450 - 3);
        assert_eq!(
            VerifiedTrimOptions::default().tail_ticks,
            DEFAULT_TAIL_TICKS
        );
        assert_eq!(
            round_tick_window(&rounds, 1, DEFAULT_TAIL_TICKS, true).unwrap(),
            (100, 637)
        );
        assert_eq!(VERIFIED_TRIM_FORMAT_VERSION, 4);
    }

    #[test]
    fn compressed_transport_suffixes_do_not_leak_into_clip_names() {
        assert_eq!(
            source_demo_stem(Path::new("C:/captures/match.one.dem.gz")).unwrap(),
            "match.one"
        );
        assert_eq!(
            source_demo_stem(Path::new("C:/captures/match.one.dem.zst")).unwrap(),
            "match.one"
        );
        assert_eq!(
            source_demo_stem(Path::new("C:/captures/match.one.dem")).unwrap(),
            "match.one"
        );
    }

    #[test]
    fn verified_publish_replaces_an_existing_clip() {
        let directory = temp_directory();
        let temp = directory.join("new.partial");
        let destination = directory.join("clip.dem");
        std::fs::write(&temp, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();

        publish_verified(&temp, &destination, true).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!temp.exists());
        assert!(!directory.join("clip.dem.replaced").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_second_rename_restores_the_existing_clip() {
        let directory = temp_directory();
        let temp = directory.join("new.partial");
        let destination = directory.join("clip.dem");
        std::fs::write(&temp, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();
        let mut calls = 0;

        let result = publish_verified_with(&temp, &destination, true, |from, to| {
            calls += 1;
            if calls == 2 {
                Err(std::io::Error::other("induced publication failure"))
            } else {
                std::fs::rename(from, to)
            }
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"old");
        assert_eq!(std::fs::read(&temp).unwrap(), b"new");
        assert!(!directory.join("clip.dem.replaced").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_backup_is_removed_before_replacing_published_clip() {
        let directory = temp_directory();
        let temp = directory.join("new.partial");
        let destination = directory.join("clip.dem");
        let backup = directory.join("clip.dem.replaced");
        std::fs::write(&temp, b"new").unwrap();
        std::fs::write(&destination, b"published-before-crash").unwrap();
        std::fs::write(&backup, b"stale-old").unwrap();

        publish_verified(&temp, &destination, true).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!temp.exists());
        assert!(!backup.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_without_destination_is_restored_before_replacement() {
        let directory = temp_directory();
        let temp = directory.join("new.partial");
        let destination = directory.join("clip.dem");
        let backup = directory.join("clip.dem.replaced");
        std::fs::write(&temp, b"new").unwrap();
        std::fs::write(&backup, b"old-before-crash").unwrap();

        publish_verified(&temp, &destination, true).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!temp.exists());
        assert!(!backup.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backup_only_recovery_preserves_old_clip_without_force() {
        let directory = temp_directory();
        let temp = directory.join("new.partial");
        let destination = directory.join("clip.dem");
        let backup = directory.join("clip.dem.replaced");
        std::fs::write(&temp, b"new").unwrap();
        std::fs::write(&backup, b"old-before-crash").unwrap();

        let result = publish_verified(&temp, &destination, false);

        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"old-before-crash");
        assert_eq!(std::fs::read(&temp).unwrap(), b"new");
        assert!(!backup.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
