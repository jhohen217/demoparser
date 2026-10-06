//! Decoding the animation pose recipe far enough to find a weapon's attack overlay.
//!
//! The recipe is a serialized `CNmTaskSystem` task list carried on two streams: a per-slot topology
//! blob giving the structure, and a payload of per-task values for whichever slot is active. See
//! ANIMTASK_FORMAT.md for how the format was read out of `client.dll`.
//!
//! The reason this exists: a weapon's firing animation is not a clip of its own. Every attempt to
//! find "the shot clip" failed, because it is a permanent overlay whose *playback time restarts* on
//! each trigger pull. The clip index never changes; a sixteen bit normalised time jumps back toward
//! zero. That is why no entity field and no user command reached it — it is a value inside the pose
//! payload, which is the one channel a suppression never touches.
//!
//! The overlay's clip differs per weapon (837 for one MAC-10, 521 for an AK-47), so it is detected
//! rather than named: the sampler whose time runs backwards on a tick the player fired.

use anyhow::Result;
use std::collections::BTreeMap;

/// Dependency count per task type id. Seventeen classes exist; only these nine occur in real
/// recipes, and a parse producing anything else is wrong rather than merely unusual.
fn dependencies(type_id: u32) -> Option<u32> {
    Some(match type_id {
        1 | 6 => 0,
        0 | 5 | 14 | 15 => 1,
        7 | 8 | 10 => 2,
        _ => return None,
    })
}

const TYPE_ID_BITS: u32 = 5;
/// The graph's clip index width, confirmed two ways: only ten bits lets every payload walk land
/// where it should, and the clip indices themselves reach 971, which needs ten bits and not nine.
pub const CLIP_ID_BITS: u32 = 10;
/// Width of a bone name inside a mask, supplied by the deserialisation context.
const MASK_NAME_BITS: u32 = 4;
/// Bone index width in the current worldmodel serialization context. The
/// type-15 task's two leading names and optional replacement names use it.
const FOOT_IK_BONE_BITS: u32 = 7;
/// The first four payload bytes are a header the game's own decoder is never shown.
const HEADER_BITS: u32 = 32;
/// Maximum payload capacity used by our recipe tools. The network vector's
/// actual length varies by recording (64 and 96 bytes are both observed).
pub const PAYLOAD_BYTES: usize = 128;

struct BitReader<'a> {
    data: &'a [u8],
    pos: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], pos: u32) -> Self {
        Self { data, pos }
    }

    /// LSB first within each byte, which is how every stream in this format reads.
    fn take(&mut self, width: u32) -> Option<u32> {
        if width == 0 {
            return Some(0);
        }
        let end = self.pos.checked_add(width)?;
        if end as usize > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u32;
        for step in 0..width {
            let at = self.pos + step;
            let bit = (self.data[(at / 8) as usize] >> (at % 8)) & 1;
            value |= (bit as u32) << step;
        }
        self.pos = end;
        Some(value)
    }

    fn skip(&mut self, width: u32) -> Option<()> {
        let end = self.pos.checked_add(width)?;
        if end as usize > self.data.len() * 8 {
            return None;
        }
        self.pos = end;
        Some(())
    }
}

/// The task type sequence a topology blob describes, or `None` if it does not parse.
///
/// A parse is only accepted when it consumes the blob exactly and every dependency names an earlier
/// task. Those two constraints together are what pinned the grammar against every distinct blob in
/// a demo.
pub fn topology(blob: &[u8]) -> Option<Vec<u32>> {
    Some(
        parse_topology(blob)?
            .tasks
            .into_iter()
            .map(|task| task.type_id)
            .collect(),
    )
}

/// One task in the list: its class, and which earlier tasks feed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub type_id: u32,
    pub deps: Vec<u32>,
}

/// A whole topology, kept in the shape needed to write one back out.
///
/// `count_bits` is carried rather than recomputed because it is the blob's own choice: the encoder
/// picks a minimal width for the task count, and reproducing a blob byte for byte means reproducing
/// the width it used, not the width we would have picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    pub count_bits: u32,
    pub tasks: Vec<Task>,
}

/// The full topology, dependencies included.
///
/// Same acceptance rules as [`topology`]: the blob must be consumed to within a byte, and every
/// dependency must name an earlier task. Those two constraints are what pinned the grammar.
pub fn parse_topology(blob: &[u8]) -> Option<Topology> {
    let bits = blob.len() as u32 * 8;
    let mut reader = BitReader::new(blob, 0);
    let count_bits = reader.take(4)?;
    if count_bits == 0 || count_bits > 6 {
        return None;
    }
    let count = reader.take(count_bits)?;
    if count == 0 {
        return None;
    }
    let mut tasks = Vec::with_capacity(count as usize);
    for index in 0..count {
        let type_id = reader.take(TYPE_ID_BITS)?;
        let how_many = dependencies(type_id)?;
        let mut deps = Vec::with_capacity(how_many as usize);
        for _ in 0..how_many {
            let dep = reader.take(count_bits)?;
            if dep >= index {
                return None;
            }
            deps.push(dep);
        }
        tasks.push(Task { type_id, deps });
    }
    if bits.saturating_sub(reader.pos) >= 8 {
        return None;
    }
    Some(Topology { count_bits, tasks })
}

/// The narrowest width that can hold a task count, which is what the game's encoder appears to use.
fn minimal_count_bits(count: usize) -> u32 {
    (1..=6)
        .find(|bits| count <= (1usize << bits) - 1)
        .unwrap_or(6)
}

/// Serialise a topology back to its blob form.
///
/// Round-tripping is exact for every blob a demo carries, which is the only reason this can be
/// trusted to write one we invented.
pub fn encode_topology(topology: &Topology) -> Result<Vec<u8>> {
    anyhow::ensure!(
        (1..=6).contains(&topology.count_bits),
        "count_bits {} out of range",
        topology.count_bits
    );
    let count = topology.tasks.len();
    anyhow::ensure!(count > 0, "a topology with no tasks is not valid");
    anyhow::ensure!(
        count <= (1usize << topology.count_bits) - 1,
        "{count} tasks will not fit in {} bits — widen count_bits first",
        topology.count_bits
    );

    let mut bits = 4 + topology.count_bits as usize;
    for task in &topology.tasks {
        bits += TYPE_ID_BITS as usize + task.deps.len() * topology.count_bits as usize;
    }
    let mut out = vec![0u8; bits.div_ceil(8)];

    let mut at = 0u32;
    write_bits(&mut out, at, 4, topology.count_bits)?;
    at += 4;
    write_bits(&mut out, at, topology.count_bits, count as u32)?;
    at += topology.count_bits;

    for (index, task) in topology.tasks.iter().enumerate() {
        // A known type must carry the dependency count the format says it does. An unknown one is
        // being probed, and its count is the thing under test, so it is written as given.
        if let Some(expected) = dependencies(task.type_id) {
            anyhow::ensure!(
                task.deps.len() as u32 == expected,
                "task {index} of type {} needs {expected} dependencies, got {}",
                task.type_id,
                task.deps.len()
            );
        }
        write_bits(&mut out, at, TYPE_ID_BITS, task.type_id)?;
        at += TYPE_ID_BITS;
        for &dep in &task.deps {
            anyhow::ensure!(
                dep < index as u32,
                "task {index} depends on {dep}, which is not an earlier task"
            );
            write_bits(&mut out, at, topology.count_bits, dep)?;
            at += topology.count_bits;
        }
    }
    Ok(out)
}

/// Append a task whose type is not one of the nine that occur in recordings.
///
/// Eight of the seventeen task classes never appear in any recipe this demo carries, so their type
/// ids and dependency counts are unknown. `CNmChainLookatTask` is one of them, and identifying it
/// is what a head look-at needs. This exists to try a candidate id directly: the id space is only
/// eight wide, and the client's reaction — accepted, ignored, or fatal — is itself the measurement.
///
/// The dependency count is supplied rather than looked up, because that is the other half of the
/// guess.
pub fn append_unknown_task(topology: &mut Topology, type_id: u32, deps: Vec<u32>) -> Result<u32> {
    anyhow::ensure!(
        type_id < (1 << TYPE_ID_BITS),
        "type id {type_id} needs more than five bits"
    );
    let index = topology.tasks.len() as u32;
    for &dep in &deps {
        anyhow::ensure!(dep < index, "dependency {dep} is not an earlier task");
    }
    topology.tasks.push(Task { type_id, deps });
    topology.count_bits = topology
        .count_bits
        .max(minimal_count_bits(topology.tasks.len()));
    Ok(index)
}

/// Append a task to the end of a topology, widening `count_bits` if the count no longer fits.
///
/// Appending is the whole trick. Dependency indices only ever name earlier tasks, so adding one at
/// the end leaves every existing entry meaning exactly what it did, and the new task's payload
/// fields land after all the existing ones rather than displacing them. Widening, when it is
/// needed, rewrites this blob and nothing else: no payload field's width derives from `count_bits`.
///
/// Returns the new task's index, which is what a later task names to consume it.
pub fn append_task(topology: &mut Topology, type_id: u32, deps: Vec<u32>) -> Result<u32> {
    let expected = dependencies(type_id)
        .ok_or_else(|| anyhow::anyhow!("type id {type_id} is not a known task"))?;
    anyhow::ensure!(
        deps.len() as u32 == expected,
        "type {type_id} needs {expected} dependencies, got {}",
        deps.len()
    );
    let index = topology.tasks.len() as u32;
    for &dep in &deps {
        anyhow::ensure!(dep < index, "dependency {dep} is not an earlier task");
    }
    topology.tasks.push(Task { type_id, deps });
    topology.count_bits = topology
        .count_bits
        .max(minimal_count_bits(topology.tasks.len()));
    Ok(index)
}

/// A bone mask: a five bit entry count, a width derived from it, then a three bit kind per entry.
/// The only variable length read in the format.
fn skip_mask(reader: &mut BitReader) -> Option<()> {
    let count = reader.take(5)?;
    if count == 0 {
        return Some(());
    }
    let index_bits = 32 - count.leading_zeros();
    for _ in 0..count {
        match reader.take(3)? {
            0 => {
                reader.take(MASK_NAME_BITS)?;
            }
            1 => {
                reader.take(8)?;
            }
            2 => {
                reader.take(2 * index_bits)?;
                reader.take(8)?;
            }
            3 => {
                reader.take(index_bits)?;
                reader.take(8)?;
            }
            4 => {
                reader.take(2 * index_bits)?;
            }
            // Kinds five and above are not handled by the game either.
            _ => return None,
        }
    }
    Some(())
}

/// Current-build type 15: two foot targets followed by an optional weight.
///
/// `client.dll`'s foot-IK deserializer reads two context bone indices, then
/// either a 48-bit rotation and three ranged shorts or a replacement bone
/// index per foot. The final flag optionally adds a flag and normalized byte.
/// The common two-transform branch consumes 209 bits, matching the AimCS
/// alignment in independent moving and stationary current-build recordings.
fn skip_foot_ik(reader: &mut BitReader) -> Option<()> {
    reader.take(FOOT_IK_BONE_BITS)?;
    reader.take(FOOT_IK_BONE_BITS)?;
    for _ in 0..2 {
        if reader.take(1)? == 0 {
            reader.skip(48 + 3 * 16)?;
        } else {
            reader.take(FOOT_IK_BONE_BITS)?;
        }
    }
    if reader.take(1)? != 0 {
        reader.take(1)?;
        reader.take(8)?;
    }
    Some(())
}

/// One clip sampler within a recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampler {
    pub clip: u32,
    /// Normalised playback position, 0..=65535.
    pub time: u32,
    /// Bit offset of the time field within the payload, which is what an edit writes to.
    pub time_at: u32,
}

/// Every sampler in a recipe, with where its time sits in the payload.
pub fn samplers(sequence: &[u32], payload: &[u8]) -> Option<Vec<Sampler>> {
    let mut reader = BitReader::new(payload, HEADER_BITS);
    let mut out = Vec::new();
    for &type_id in sequence {
        // CNmSampleTask is the one task this walk needs to look inside; the rest are stepped over
        // by the shared width table so no second definition of a field layout can drift from it.
        if type_id == 1 {
            let clip = reader.take(CLIP_ID_BITS)?;
            let time_at = reader.pos;
            let time = reader.take(16)?;
            out.push(Sampler {
                clip,
                time,
                time_at,
            });
        } else {
            skip_task(type_id, &mut reader)?;
        }
    }
    Some(out)
}

/// Advance past one task's payload fields.
///
/// This is the single definition of every task's payload width. Each walk over the payload goes
/// through it, so a field layout cannot be stated in one place and contradicted in another.
fn skip_task(type_id: u32, reader: &mut BitReader) -> Option<()> {
    match type_id {
        // CNmSampleTask: a clip index, then a sixteen bit normalised time.
        1 => {
            reader.take(CLIP_ID_BITS)?;
            reader.take(16)?;
        }
        // All four blend classes share one deserialiser: a weight, a flag, then a mask if set.
        7 | 8 | 10 => {
            reader.take(8)?;
            if reader.take(1)? != 0 {
                skip_mask(reader)?;
            }
        }
        // CNmSnapWeaponTask.
        5 => {
            reader.take(2)?;
        }
        // CNmAimCSTask: 16+16+8+8+8+8, then a three and a five bit enum.
        14 => {
            for width in [16, 16, 8, 8, 8, 8, 3, 5] {
                reader.take(width)?;
            }
        }
        // CNmCachedPoseWriteTask.
        0 => {
            reader.take(6)?;
        }
        // Foot IK; its common current-build branch is 209 bits. The former
        // 2-bit placeholder misaligned the following AimCS and SnapWeapon.
        15 => {
            skip_foot_ik(reader)?;
        }
        // Type 6 takes no payload at all.
        6 => {}
        _ => return None,
    }
    Some(())
}

/// The bit position just past the fields of the task classes decoded here.
/// The network vector can contain additional data after this position; do not
/// treat it as the full length or overwrite trailing bytes when editing values.
pub fn payload_end(sequence: &[u32], payload: &[u8]) -> Option<u32> {
    let mut reader = BitReader::new(payload, HEADER_BITS);
    for &type_id in sequence {
        skip_task(type_id, &mut reader)?;
    }
    Some(reader.pos)
}

/// Append a `CNmSampleTask`'s payload: a clip index and a sixteen bit normalised time.
///
/// Returns the bit position after the write, which is where the next appended task begins.
pub fn append_sample_payload(payload: &mut [u8], at: u32, clip: u32, time: u32) -> Result<u32> {
    anyhow::ensure!(
        clip < (1 << CLIP_ID_BITS),
        "clip {clip} needs more than {CLIP_ID_BITS} bits"
    );
    anyhow::ensure!(
        time <= u16::MAX as u32,
        "time {time} does not fit in sixteen bits"
    );
    write_bits(payload, at, CLIP_ID_BITS, clip)?;
    write_bits(payload, at + CLIP_ID_BITS, 16, time)?;
    Ok(at + CLIP_ID_BITS + 16)
}

/// Append a blend task's payload: an eight bit weight, then either no mask or a single named one.
///
/// `mask` names an entry in the skeleton's mask list — the `.vnmskel` defines twelve of them and
/// the name field is four bits wide, so there is room for a few more. A named mask is written as
/// the one entry kind the game's own deserialiser treats as a name: a count of one, kind zero, and
/// the four bit index.
pub fn append_blend_payload(
    payload: &mut [u8],
    at: u32,
    weight: u32,
    mask: Option<u32>,
) -> Result<u32> {
    anyhow::ensure!(
        weight <= u8::MAX as u32,
        "weight {weight} does not fit in eight bits"
    );
    write_bits(payload, at, 8, weight)?;
    let mut end = at + 8;
    match mask {
        None => {
            write_bits(payload, end, 1, 0)?;
            end += 1;
        }
        Some(name) => {
            anyhow::ensure!(
                name < (1 << MASK_NAME_BITS),
                "mask name {name} needs more than {MASK_NAME_BITS} bits"
            );
            write_bits(payload, end, 1, 1)?;
            end += 1;
            write_bits(payload, end, 5, 1)?; // one entry
            end += 5;
            write_bits(payload, end, 3, 0)?; // entry kind zero: a mask named by index
            end += 3;
            write_bits(payload, end, MASK_NAME_BITS, name)?;
            end += MASK_NAME_BITS;
        }
    }
    Ok(end)
}

/// Where the aim task's two sixteen bit values sit in the payload.
///
/// `CNmAimCSTask` is in every recipe, and its payload opens with two sixteen bit values remapped as
/// `v * scale - offset` — the shape of a signed angle pair, and both sweep either side of the 32768
/// midpoint in a recording. Writing them steers the upper body without touching the task list.
pub fn aim_fields(sequence: &[u32], payload: &[u8]) -> Option<(u32, u32)> {
    let mut reader = BitReader::new(payload, HEADER_BITS);
    for &type_id in sequence {
        match type_id {
            1 => {
                reader.take(CLIP_ID_BITS)?;
                reader.take(16)?;
            }
            7 | 8 | 10 => {
                reader.take(8)?;
                if reader.take(1)? != 0 {
                    skip_mask(&mut reader)?;
                }
            }
            5 => {
                reader.take(2)?;
            }
            14 => {
                let first = reader.pos;
                reader.take(16)?;
                let second = reader.pos;
                return Some((first, second));
            }
            0 => {
                reader.take(6)?;
            }
            15 => {
                skip_foot_ik(&mut reader)?;
            }
            6 => {}
            _ => return None,
        }
    }
    None
}

/// Write an arbitrary width value at a bit offset.
pub fn write_bits(payload: &mut [u8], at: u32, width: u32, value: u32) -> Result<()> {
    anyhow::ensure!(
        at as usize + width as usize <= payload.len() * 8,
        "field runs past the payload"
    );
    for step in 0..width {
        let bit = ((value >> step) & 1) as u8;
        let pos = at + step;
        let byte = &mut payload[(pos / 8) as usize];
        let mask = 1u8 << (pos % 8);
        *byte = (*byte & !mask) | (bit << (pos % 8));
    }
    Ok(())
}

/// Write an eight bit value at a bit offset.
pub fn write_u8(payload: &mut [u8], at: u32, value: u32) -> Result<()> {
    anyhow::ensure!(
        at as usize + 8 <= payload.len() * 8,
        "byte runs past the payload"
    );
    for step in 0..8u32 {
        let bit = ((value >> step) & 1) as u8;
        let pos = at + step;
        let byte = &mut payload[(pos / 8) as usize];
        let mask = 1u8 << (pos % 8);
        *byte = (*byte & !mask) | (bit << (pos % 8));
    }
    Ok(())
}

/// Read a sixteen bit value at a bit offset.
pub fn read_u16(payload: &[u8], at: u32) -> Option<u32> {
    if at as usize + 16 > payload.len() * 8 {
        return None;
    }
    let mut value = 0u32;
    for step in 0..16u32 {
        let pos = at + step;
        value |= (((payload[(pos / 8) as usize] >> (pos % 8)) & 1) as u32) << step;
    }
    Some(value)
}

/// Write a sixteen bit value at a bit offset, leaving every length alone.
pub fn write_u16(payload: &mut [u8], at: u32, value: u32) -> Result<()> {
    anyhow::ensure!(
        at as usize + 16 <= payload.len() * 8,
        "field runs past the payload"
    );
    for step in 0..16u32 {
        let bit = ((value >> step) & 1) as u8;
        let pos = at + step;
        let byte = &mut payload[(pos / 8) as usize];
        let mask = 1u8 << (pos % 8);
        *byte = (*byte & !mask) | (bit << (pos % 8));
    }
    Ok(())
}

/// An attack overlay starting to play.
#[derive(Debug, Clone, Copy)]
pub struct Restart {
    pub clip: u32,
    /// The time it held a tick ago, or `None` when the sampler is new to the recipe.
    pub was: Option<u32>,
    pub now: u32,
    pub time_at: u32,
}

impl Restart {
    /// Whether the overlay appeared rather than rewound.
    ///
    /// Both are the animation starting. A retrigger rewinds a sampler already in the recipe; the
    /// first shot of a burst instead *adds* the sampler, so there is no earlier time to compare
    /// against and nothing rewinds. Watching only for rewinds misses exactly one shot per burst,
    /// which is precisely what survived the first version of this.
    pub fn is_new(&self) -> bool {
        self.was.is_none()
    }
}

/// How far into a clip a freshly added sampler may be and still count as one just starting. An
/// overlay that appears already part-played is something continuing, not a shot being fired.
const FRESH_TIME_MAX: u32 = 8192;

/// Follows one entity's recipe across ticks and reports when an overlay restarts.
#[derive(Default)]
pub struct PoseTracker {
    slots: BTreeMap<u32, Vec<u8>>,
    active: Option<u32>,
    /// The payload as it now stands. Writes are sparse, so it has to be carried forward: a tick
    /// rewrites a handful of the bytes and the rest keep their previous values.
    payload: Vec<u8>,
    previous: BTreeMap<u32, u32>,
}

impl PoseTracker {
    pub fn set_topology(&mut self, slot: u32, blob: Vec<u8>) {
        self.slots.insert(slot, blob);
    }

    pub fn set_active(&mut self, slot: u32) {
        self.active = Some(slot);
    }

    pub fn set_byte(&mut self, index: usize, value: u8) {
        if index >= PAYLOAD_BYTES {
            return;
        }
        if self.payload.len() <= index {
            self.payload.resize(index + 1, 0);
        }
        self.payload[index] = value;
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Which slot's task list the client is currently applying.
    pub fn active_slot(&self) -> Option<u32> {
        self.active
    }

    /// A slot's topology blob as last transmitted.
    pub fn topology_blob(&self, slot: u32) -> Option<&[u8]> {
        self.slots.get(&slot).map(Vec::as_slice)
    }

    /// The task list of the active slot, which the aim walk needs as well as the sampler walk.
    pub fn sequence(&self) -> Option<Vec<u32>> {
        topology(self.slots.get(&self.active?)?)
    }

    pub fn samplers(&self) -> Option<Vec<Sampler>> {
        let blob = self.slots.get(&self.active?)?;
        samplers(&topology(blob)?, &self.payload)
    }

    /// Advance to the current state, reporting any sampler whose time went backwards.
    pub fn step(&mut self) -> Vec<Restart> {
        let Some(found) = self.samplers() else {
            return Vec::new();
        };
        let mut restarts = Vec::new();
        for sampler in &found {
            match self.previous.get(&sampler.clip) {
                Some(&was) if sampler.time < was => restarts.push(Restart {
                    clip: sampler.clip,
                    was: Some(was),
                    now: sampler.time,
                    time_at: sampler.time_at,
                }),
                Some(_) => {}
                // New to the recipe and barely started: an overlay being introduced to play.
                None if !self.previous.is_empty() && sampler.time <= FRESH_TIME_MAX => restarts
                    .push(Restart {
                        clip: sampler.clip,
                        was: None,
                        now: sampler.time,
                        time_at: sampler.time_at,
                    }),
                None => {}
            }
        }
        self.previous = found.iter().map(|s| (s.clip, s.time)).collect();
        restarts
    }
}

/// The payload with one time field overwritten. Every length is left unchanged, which is the only
/// edit shape this payload has ever accepted: withholding bytes crashed the client twice.
pub fn write_time(payload: &mut [u8], time_at: u32, value: u32) -> Result<()> {
    anyhow::ensure!(
        time_at as usize + 16 <= payload.len() * 8,
        "time field runs past the payload"
    );
    for step in 0..16u32 {
        let at = time_at + step;
        let bit = ((value >> step) & 1) as u8;
        let byte = &mut payload[(at / 8) as usize];
        let mask = 1u8 << (at % 8);
        *byte = (*byte & !mask) | (bit << (at % 8));
    }
    Ok(())
}

/// The clip index sits immediately before the time field it belongs to.
pub fn clip_at(time_at: u32) -> u32 {
    time_at - CLIP_ID_BITS
}

/// Rewrite a sampler's clip index in place.
///
/// Ten bits, and the payload is a fixed size buffer, so nothing about any length changes — which is
/// the only edit shape this data has ever accepted.
pub fn write_clip(payload: &mut [u8], time_at: u32, clip: u32) -> Result<()> {
    let at = clip_at(time_at);
    anyhow::ensure!(
        at as usize + CLIP_ID_BITS as usize <= payload.len() * 8,
        "clip index runs past the payload"
    );
    for step in 0..CLIP_ID_BITS {
        let bit = ((clip >> step) & 1) as u8;
        let pos = at + step;
        let byte = &mut payload[(pos / 8) as usize];
        let mask = 1u8 << (pos % 8);
        *byte = (*byte & !mask) | (bit << (pos % 8));
    }
    Ok(())
}

/// Which payload bytes a ten bit clip index at `time_at` occupies.
pub fn clip_bytes(time_at: u32) -> std::ops::Range<usize> {
    let at = clip_at(time_at);
    (at / 8) as usize..((at + CLIP_ID_BITS - 1) / 8) as usize + 1
}

/// Which payload bytes a sixteen bit time field at `time_at` occupies.
pub fn time_bytes(time_at: u32) -> std::ops::Range<usize> {
    let first = (time_at / 8) as usize;
    let last = ((time_at + 15) / 8) as usize;
    first..last + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_write_changes_only_its_own_bits() {
        let mut payload = vec![0xFFu8; 16];
        let before = payload.clone();
        write_time(&mut payload, 37, 0x1234).unwrap();
        let touched = time_bytes(37);
        for (index, (now, was)) in payload.iter().zip(&before).enumerate() {
            if !touched.contains(&index) {
                assert_eq!(now, was, "byte {index} changed outside the time field");
            }
        }
        let read = samplers(&[], &payload);
        assert!(read.is_some());
    }

    #[test]
    fn a_written_time_reads_back() {
        let mut payload = vec![0u8; 32];
        write_time(&mut payload, HEADER_BITS + CLIP_ID_BITS, 0xBEEF).unwrap();
        let found = samplers(&[1], &payload).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].time, 0xBEEF);
        assert_eq!(found[0].time_at, HEADER_BITS + CLIP_ID_BITS);
    }

    #[test]
    fn topology_rejects_a_blob_it_cannot_consume_exactly() {
        // A count promising more tasks than the blob can hold must not parse.
        assert!(topology(&[0xFF, 0xFF]).is_none());
        assert!(topology(&[]).is_none());
    }
}

#[cfg(test)]
#[path = "poserecipe_topology_tests.rs"]
mod topology_tests;
