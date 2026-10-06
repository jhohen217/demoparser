# S2R Format — S2 Replay Binary Format

> **Location in project:** `src/demoparser_rust/src/tick_by_tick/s2r_output.rs` (writer), `src/demoparser_rust/CollectionBrowser/src/s2r_loader.rs` (reader)

---

## Overview

### v17 diagnostic death-input provenance

The v17 writer introduced container v17 with the v16 binary utility authority
(S2EX tag 12/schema 2) and a diagnostic-only death-input provenance lane
(tag 14/schema 1). Tag 14 keeps the selected dead-row source tick separate
from the kill-event tick and retains nullable `m_vRagdollServerOrigin`.
Readers may inspect it for parity diagnostics; replay physics does not consume it.

### v18 networked AG2 recipes

The current writer emits container v18 and S2EX tag 15/schema 1. Tag 15 retains complete
networked player-pawn recipe snapshots: every slot topology, the entire dynamic byte vector,
active slot, recipe version, graph definition and graph iteration where observed. The parser
preserves these as opaque serialized task-system input; it does not decode recipes or include
opaque `DEM_AnimationData` / `DEM_AnimationHeader` records.

### v16 utility performance extension

The v16 writer introduced container v16 and S2EX tag 12/schema 2: lossless
dictionary-coded typed utility records, default LZ4 block compression, explicit
uncompressed length and IEEE CRC32. The other section layouts are unchanged.
v15/tag 12/schema 1 JSON remains readable by S2DVR but is not a current writer
cache hit. The exact wire schema, benchmarks, limits and DuckDB audit contract
are covered by the writer implementation; external consumer benchmarks are not packaged here.
Capability: `s2r-utility-binary-v2` in addition to `s2r-utility-authority-v1`.

## File Layout

All values are **little-endian**. Offsets for every section are stored in the header.

```
[Header          — 64 bytes fixed]
[Players block   — player_count × 46 bytes]
[Meta block      — 4-byte length prefix + JSON payload]
[Frames block    — variable size, alive-only frames per player]
[Kill events     — 2-byte count + kill_count × 12 bytes]
[Util events     — 2-byte count + util_count × 24 bytes]
[Trajectories    — 4-byte count + variable-length per grenade]
[Weapon Fire     — 2-byte count + variable-length CSR rows]
[Raw audio       — v7: u32 count + length-delimited records]
[Smoke voxels    — v8: u32 track count + entity-lifetime tracks]
[S2EX extensions — v9+: versioned directory + sparse authority sections]
```

---

## Section Reference

### 1. Header — 64 bytes

| Offset | Size | Type     | Field            | Notes |
|--------|------|----------|------------------|-------|
| 0      | 4    | `[u8;4]` | `magic`          | Always `"S2RF"` |
| 4      | 2    | `u16`    | `version`        | Currently `15` |
| 6      | 1    | `u8`     | `player_count`   | Max 255 |
| 7      | 1    | `u8`     | `_pad`           | Reserved |
| 8      | 4    | `u32`    | `tick_count`     | Total ticks in range (for reference) |
| 12     | 4    | `i32`    | `min_tick`       | Lowest tick number in this collection |
| 16     | 4    | `u32`    | `players_offset` | Byte offset to Players block |
| 20     | 4    | `u32`    | `meta_offset`    | Byte offset to Meta block |
| 24     | 4    | `u32`    | `frames_offset`  | Byte offset to Frames block |
| 28     | 4    | `u32`    | `kills_offset`   | Byte offset to Kill events block |
| 32     | 4    | `u32`    | `util_offset`    | Byte offset to Utility events block |
| 36     | 2    | `u16`    | `frame_stride`   | Bytes per alive frame (use this to seek) |
| 38     | 4    | `u32`    | `traj_offset`    | Byte offset to Trajectories block (v4+) |
| 42     | 4    | `u32`    | `wf_offset`      | Byte offset to Weapon Fire block (v4+) |
| 46     | 4    | `u32`    | `audio_offset`   | Byte offset to Raw audio block (v7+ only) |
| 50     | 4    | `u32`    | `smoke_offset`   | Byte offset to Smoke voxels block (v8+ only) |
| 54     | 4    | `u32`    | `extension_offset` | Byte offset to `S2EX` (v9+) |
| 58     | 4    | `u32`    | `extension_length` | Directory plus payload bytes |
| 62     | 2    | `[u8;2]` | `_reserved`      | Zero-padded |

---

### 2. Players Block — `player_count × 46 bytes`

One entry per player, sorted by Steam ID for determinism.

| Offset | Size | Type      | Field         | Notes |
|--------|------|-----------|---------------|-------|
| 0      | 8    | `u64`     | `steamid`     | Steam 64-bit ID |
| 8      | 32   | `[u8;32]` | `name`        | Null-terminated UTF-8, zero-padded |
| 40     | 1    | `u8`      | `team`        | `2`=T, `3`=CT, `0`=unknown |
| 41     | 2    | `u16`     | `frame_count` | Number of alive frames for this player (v3+) |
| 43     | 3    | `[u8;3]`  | `_pad`        | Reserved |

> **v2 files:** Entry is 44 bytes (no `frame_count` or `_pad`).

---

### 3. Meta Block — `4 + meta_len bytes`

| Size        | Type     | Field      | Notes |
|-------------|----------|------------|-------|
| 4           | `u32`    | `meta_len` | Byte length of the JSON that follows |
| `meta_len`  | `[u8;*]` | JSON       | UTF-8 encoded JSON object |

#### Meta JSON Fields

| Field                      | Type     | Description |
|----------------------------|----------|-------------|
| `version`                  | int      | Meta schema version (`1`) |
| `collection_type`          | string   | e.g. `"ACE"`, `"4k"`, `"3k"` |
| `collection_num`           | int      | Sequential index of this collection in the demo |
| `tick_duration`            | int      | Number of ticks in the collection window |
| `map_name`                 | string   | CS2 map name, e.g. `"de_dust2"` |
| `game_version`             | int      | CS2 build version |
| `killer_index`             | int      | Player array index of the killer |
| `killer_team`              | string   | `"T"` or `"CT"` |
| `start_tick`               | int      | First kill tick (collection start) |
| `end_tick`                 | int      | Last kill tick (collection end) |
| `killer_name`              | string   | Display name of the killer |
| `killer_steamid`           | string   | Steam ID as decimal string |
| `demo_name`                | string   | Source demo filename |
| `folder`                   | string   | Source folder path |
| `killer_radius`            | float    | Movement radius of the killer |
| `victims_radius`           | float    | Spread radius of victims |
| `killer_move_distance`     | float    | Total distance moved by killer |
| `victim_team`              | string   | Team of the victims |
| `round_start_tick`         | int      | Tick when the round started |
| `round_end_tick`           | int      | Tick when the round ended |
| `round_freeze_end`         | int      | Tick when freeze time ended |
| `round`                    | int      | Round number |
| `weapons`                  | string   | Semicolon-delimited weapon names |
| `weapons_id`               | string   | Semicolon-delimited weapon IDs |
| `kill_ticks`               | string   | Semicolon-delimited kill tick list, e.g. `"[45230;45350]"` |
| `victims_index`            | string   | Semicolon-delimited victim player indices |
| `weapon_switch_ticks`      | int[]    | Ticks where the killer switched weapons |
| `padding`                  | int      | Pre-kill padding ticks added (`-1` if none) |
| `ticks_tracked`            | int      | Alias for `tick_duration` |
| `game_start_offset`        | int      | Offset from demo start to game start tick |
| `parsed`                   | int      | Always `1` when written |
| `hits`                     | int      | Grenade hits by killer |
| `misses`                   | int      | Grenade misses by killer |
| `hit_rate`                 | float    | Grenade hit rate (0.0–1.0) |
| `util_thrown`              | int      | Grenades thrown count |
| `util_thrown_ticks`        | int[]    | Ticks when grenades were thrown |
| `util_land_ticks`          | int[]    | Ticks when grenades landed |
| `weapons_damaged`          | string[] | Weapon names involved in grenade hits |
| `weapons_damaged_num_hits` | int[]    | Hit counts per weapon |
| `total_ticks`              | int      | Total ticks in the full demo |
| `players`                  | object[] | Player list (see below) |

#### `players` Array Entry

```json
{
  "steamid": "76561198012345678",
  "name": "PlayerName",
  "index": 0,
  "team": "CT"
}
```

---

### 4. Frames Block — Alive-only, variable length

Frames are stored **per-player** in player-index order. Only ticks where the player was alive are stored. The reader uses `frame_count` from the Players block to know how many frames to read for each player.

#### v6 Frame (current) — 47 bytes total

| Offset | Size | Type  | Field        | Notes |
|--------|------|-------|--------------|-------|
| 0      | 4    | `i32` | `tick`       | Absolute game tick number |
| 4      | 4    | `f32` | `pos_x`      | World X position (CS2 units) |
| 8      | 4    | `f32` | `pos_y`      | World Y position |
| 12     | 4    | `f32` | `pos_z`      | World Z position |
| 16     | 4    | `f32` | `yaw`        | View yaw in degrees (v5: raw f32, was i16 in v3/v4) |
| 20     | 4    | `f32` | `pitch`      | View pitch in degrees (v5: raw f32, was i16 in v3/v4) |
| 24     | 2    | `u16` | `weapon_id`  | Numeric item-def weapon index |
| 26     | 2    | `u16` | `flags`      | Player state bitfield (see below) |
| 28     | 1    | `u8`  | `ammo`       | Current ammo count |
| 29     | 1    | `u8`  | `health`     | Player health (1–100) |
| 30     | 1    | `u8`  | `armor`      | Armor value (0–100) |
| 31     | 2    | `i16` | `vel_x`      | Velocity X × 10 (divide by 10 to get units/s) |
| 33     | 2    | `i16` | `vel_y`      | Velocity Y × 10 |
| 35     | 2    | `i16` | `vel_z`      | Velocity Z × 10 |
| 37     | 2    | `i16` | `mouse_vel`  | Mouse velocity × 10 (degrees/tick) |
| 39     | 4    | `u32` | `weapon_cosmetic_index` | Index into `meta.cosmetics.weapon_signatures`; `0` = not observed/unknown |
| 43     | 4    | `u32` | `glove_cosmetic_index` | Index into `meta.cosmetics.glove_signatures`; `0` = not observed/unknown |

The first 39 bytes of a v6 frame are exactly the v5 frame. The two indexes are
little-endian and are appended only in v6. Consumers must not interpret index
zero as a default paint kit: it exclusively means that the source demo did not
provide a complete, verified cosmetic identity for that frame.

#### Cosmetic metadata (`meta.cosmetics`, v6+)

Cosmetic signatures are normalized in the existing JSON metadata block, rather
than duplicated in every frame. `weapon_signatures` and `glove_signatures` are
JSON arrays whose element position equals the signature's explicit `index`.
Element zero is `null` and is the unknown sentinel. Every non-null weapon
signature has `item_definition_index` and `paint_kit_id`; optional fields are
only included when explicitly observed: `item_id`, `paint_seed`, `wear`,
`quality`, `stattrak`, `custom_name`, and `stickers` (`sticker_id`, optional
`wear`, `x`, `y`). Glove signatures use the same core identity fields except
stickers/StatTrak/custom name. Non-finite float observations are omitted.

Example:

```json
"cosmetics": {
  "schema_version": 1,
  "unknown_index": 0,
  "weapon_signatures": [null, {
    "index": 1,
    "item_definition_index": 7,
    "paint_kit_id": 180,
    "paint_seed": 42,
    "wear": 0.123
  }],
  "glove_signatures": [null]
}
```

> **v3/v4 frames** are 35 bytes. Angles are stored as `i16` scaled by `182.0444`
> (`degrees × 182.044` → `i16`; divide by `182.044` to recover degrees).

#### Flags Bitfield (`u16`)

| Bit | Name         | Meaning |
|-----|--------------|---------|
| 0   | `in_reload`  | Player is reloading |
| 1   | `scoped`     | Player is scoped in |
| 2   | `inspecting` | Player is inspecting weapon |
| 3   | `airborne`   | Player is in the air |
| 4   | `walking`    | Player is walking (shift-walk) |
| 5   | `defusing`   | Player is defusing |
| 6   | `fw`         | Forward key held |
| 7   | `lf`         | Left key held |
| 8   | `rt`         | Right key held |
| 9   | `bk`         | Back key held |
| 10  | `fire`       | Fire button held |
| 11  | `crouching`  | Player is crouching |

---

### 5. Kill Events Block — `2 + kill_count × 12 bytes`

| Offset | Size | Type  | Field         | Notes |
|--------|------|-------|---------------|-------|
| 0      | 2    | `u16` | `count`       | Number of kill events |

Each kill entry (12 bytes):

| Offset | Size | Type   | Field         | Notes |
|--------|------|--------|---------------|-------|
| 0      | 4    | `i32`  | `tick`        | Game tick of the kill |
| 4      | 1    | `u8`   | `killer_idx`  | Player array index of killer |
| 5      | 1    | `u8`   | `victim_idx`  | Player array index of victim |
| 6      | 2    | `u16`  | `weapon_id`   | Weapon used |
| 8      | 1    | `u8`   | `kill_flags`  | `bit0`=headshot, `bit1`=through smoke, `bit2`=noscope, `bit3`=attacker blind, `bit4`=wallbang, `bit5`=attacker airborne, `bit6`=victim airborne |
| 9      | 1    | `u8`   | `known_flags` | v13: same bits; unset means unknown, not false |
| 10     | 2    | `u16`  | `penetration_count` | v13: surfaces penetrated; `65535` unknown/unrepresentable |

All flags can coexist. Wallbang is the death event's integer `penetrated > 0`;
legacy boolean events supply the flag but no surface count. Airborne describes
state at the death event, not necessarily at projectile launch. The preferred
source is the event's attached `attacker_is_airborne`/`user_is_airborne`; fallback
uses an alive observation at that tick or the immediately preceding tick.
Collection rows are matched to deaths by tick, killer, and victim identity.
Missing events do not become confirmed negatives. Before v13, bytes 9–11 were
padding and cannot be interpreted as a known mask or penetration count.

---

### 6. Utility Events Block — `2 + util_count × 24 bytes`

| Offset | Size | Type  | Field        | Notes |
|--------|------|-------|--------------|-------|
| 0      | 2    | `u16` | `count`      | Number of utility throw events |

Each utility entry (24 bytes):

| Offset | Size | Type   | Field          | Notes |
|--------|------|--------|----------------|-------|
| 0      | 4    | `i32`  | `tick_throw`   | Tick when grenade was thrown |
| 4      | 4    | `i32`  | `tick_land`    | Tick when grenade landed/detonated |
| 8      | 1    | `u8`   | `thrower_idx`  | Player array index of thrower |
| 9      | 1    | `u8`   | `type`         | `1`=flash, `2`=HE, `3`=smoke, `4`=molotov/incendiary, `5`=decoy, `0`=unknown |
| 10     | 2    | `u16`  | `entity_id`    | Grenade entity ID (v5), was padding in v3/v4 |
| 12     | 4    | `f32`  | `land_pos_x`   | Landing X world position |
| 16     | 4    | `f32`  | `land_pos_y`   | Landing Y world position |
| 20     | 4    | `f32`  | `land_pos_z`   | Landing Z world position |

---

### 7. Grenade Trajectories Block — `4 + variable bytes`

| Offset | Size | Type  | Field   | Notes |
|--------|------|-------|---------|-------|
| 0      | 4    | `u32` | `count` | Number of trajectories |

Each trajectory entry (v5 header = 8 bytes + `point_count × 16` bytes):

| Offset | Size | Type   | Field          | Notes |
|--------|------|--------|----------------|-------|
| 0      | 1    | `u8`   | `thrower_idx`  | Player array index |
| 1      | 1    | `u8`   | `type`         | Same type codes as Util Events |
| 2      | 2    | `u16`  | `point_count`  | Number of trajectory points |
| 4      | 4    | `u32`  | `entity_id`    | Grenade entity ID (v5) |

Each trajectory point (16 bytes):

| Offset | Size | Type  | Field   | Notes |
|--------|------|-------|---------|-------|
| 0      | 4    | `i32` | `tick`  | Game tick at this point |
| 4      | 4    | `f32` | `pos_x` | World X |
| 8      | 4    | `f32` | `pos_y` | World Y |
| 12     | 4    | `f32` | `pos_z` | World Z |

---

### 8. Weapon Fire Block — `2 + variable bytes` (v4+)

Stores per-shot events in CSR (Compressed Sparse Row) format — each shot can hit zero or more victims.

| Offset | Size | Type  | Field   | Notes |
|--------|------|-------|---------|-------|
| 0      | 2    | `u16` | `count` | Number of weapon fire events |

Each weapon fire event (variable length):

| Offset | Size | Type  | Field           | Notes |
|--------|------|-------|-----------------|-------|
| 0      | 4    | `i32` | `tick`          | Tick the shot was fired |
| 4      | 4    | `i32` | `impact_tick`   | Tick bullet impacted (`-1` = no registered impact) |
| 8      | 1    | `u8`  | `attacker_idx`  | Player array index of shooter |
| 9      | 2    | `u16` | `weapon_id`     | Weapon used |
| 11     | 1    | `u8`  | `victim_count`  | Number of victims hit (0 = miss) |

Per victim (4 bytes each, repeated `victim_count` times):

| Offset | Size | Type  | Field        | Notes |
|--------|------|-------|--------------|-------|
| 0      | 1    | `u8`  | `victim_idx` | Player array index (`255`=unknown/world) |
| 1      | 2    | `u16` | `damage`     | Damage dealt |
| 3      | 1    | `u8`  | `is_kill`    | `1`=killed, `0`=not killed |

> v3 files do not have this block; the loader falls back to synthesizing weapon fire data from the Kill Events block.

---

### 9. Raw Audio Block — variable length (v7+)

`audio_offset` points exactly to the first byte after the Weapon Fire CSR rows.
The block is produced only from the authoritative grenade/event parser lane and
is filtered to the collection replay range. It preserves protocol presence and
opaque packed bytes; it does not infer playable sound parameters.

```text
event_count: u32
repeat event_count times:
  tag:         u8
  flags:       u8     (must be zero in v7)
  reserved:    u16    (must be zero in v7)
  payload_len: u32
  payload:     [u8; payload_len]
```

Every **known v7 tag** has this 16-byte payload prefix:

```text
tick:                  i32
demo_frame_offset:     u64   // absolute byte offset of enclosing DEM frame
network_message_index: u32   // zero-based message position inside the frame
```

The producer globally merges and orders records by
`(demo_frame_offset, network_message_index)`. The key is strictly unique; a
repeated `svc_Sounds` packet remains one record containing its repeated entries
in protobuf/vector order. This avoids a parser-local counter that could vary by
chunk or thread. Unknown future tags are skipped using `payload_len` alone.

| Tag | Source message | Bytes following common prefix |
|-----|----------------|-------------------------------|
| 1 | `svc_Sounds` | `presence:u8` bit0 `reliable_sound`; optional `reliable_sound:u8`; `sound_count:u32`; then entries below |
| 2 | `svc_StopSound` | `presence:u8` bit0 `guid`; optional `guid:u32` |
| 3 | `GE_SosStartSoundEvent` | `presence:u8`; optional fields in bits 0–5 order: `soundevent_guid:i32`, `soundevent_hash:u32`, `source_entity_index:i32`, `seed:i32`, `packed_params:(u32 length + bytes)`, `start_time:f32` |
| 4 | `GE_SosStopSoundEvent` | `presence:u8` bit0 `soundevent_guid`; optional `i32` |
| 5 | `GE_SosStopSoundEventHash` | `presence:u8`; bit0 `soundevent_hash:u32`, bit1 `source_entity_index:i32` |
| 6 | `GE_SosSetSoundEventParams` | `presence:u8`; bit0 `soundevent_guid:i32`, bit1 `packed_params:(u32 length + bytes)` |
| 7 | `GE_SosSetLibraryStackFields` | `presence:u8`; bit0 `stack_hash:u32`, bit1 `packed_fields:(u32 length + bytes)` |

An `svc_Sounds` entry begins `presence:u32`; set bits serialize their values in
this exact field order. `i32`, `u32`, and `f32` values consume four bytes;
booleans consume one byte; `sound_resource_id` consumes eight bytes.

| Presence bit | Field | Type |
|---|---|---|
| 0–2 | `origin_x`, `origin_y`, `origin_z` | `i32` |
| 3 | `volume` | `u32` |
| 4 | `delay_value` | `f32` |
| 5–9 | `sequence_number`, `entity_index`, `channel`, `pitch`, `flags` | `i32` |
| 10–11 | `sound_num`, `sound_num_handle` | `u32` |
| 12–14 | `speaker_entity`, `random_seed`, `sound_level` | `i32` |
| 15–16 | `is_sentence`, `is_ambient` | `u8` (`0`/`1`) |
| 17 | `guid` | `u32` |
| 18 | `sound_resource_id` | `u64` |

Presence bit zero means **absent**, which is distinct from an explicitly
serialized zero/default value. The byte blobs above are preserved verbatim and
are intentionally not decoded by this schema.

---

### 10. Smoke Voxel Block — variable length (v8+)

`smoke_offset` points to the first byte after the v7 raw-audio block. Each track
represents one `CSmokeGrenadeProjectile` entity lifetime. Frame payloads preserve
the demo's append-only `m_VoxelFrameData` byte vector exactly; consumers decode
the cumulative payload into the 32³ occupancy grid.

```text
track_count: u32
repeat track_count times:
  entity_id:   i32
  life_index:  u16
  reserved:    u16    (zero)
  frame_count: u32
  start_tick:  i32
  end_tick:    i32
  repeat frame_count times:
    tick:                  i32
    voxel_update:          u32
    flags:                 u8
    reserved:              [u8;3] (zero)
    smoke_effect_tick:     i32
    detonation_position:   [f32;3]
    smoke_color_rgb8:      [f32;3]
    payload_length:        u32
    voxel_frame_data:      [u8; payload_length]
```

Tracks overlapping a collection retain their lifetime prefix through the
collection end. This is required because a collection may begin after the smoke
started and the payload is cumulative rather than an independent per-frame blob.

---

### 11. S2EX Authority Extensions (v9+)

`extension_offset` points to an `S2EX` header. Offsets in its directory are
relative to that header. Unknown tags or schemas are skipped.

```text
magic: "S2EX"; directory_version: u16 = 1; section_count: u16
repeat section_count: tag:u16, schema:u16, offset:u32, length:u32, count:u32
```

Schema-1 tags are:

| Tag | Section | Encoding |
|-----|---------|----------|
| 1 | Agent lives | `count:u32`, then 24-byte rows: player `u8`, flags `u8`, life ordinal `u16`, start/end-exclusive `i32`, pawn entity `u32`, character definition `u32`, pawn serial `u32` |
| 2 | Weapon lifetimes | `count:u32`, then entity index+serial, tick range, item definition, cosmetic catalog index, item id, and length-prefixed UTF-8 class name |
| 3 | Inventory deltas | `count:u32`, then 20-byte change rows: tick, lifetime id, owner player, gameplay slot, flags, clip, reserve, econ inventory position |
| 4 | World-weapon deltas | `count:u32`, then 48-byte create/update/delete rows with raw PVS transition, position, rotation, and derived velocity |
| 5 | Ragdoll impacts | `count:u32`, then 36-byte rows: kill tick `i32`, victim player `u8`, presence flags `u8` (bone/position/force), reserved `u16`, skeleton bone `i32`, world position `[f32;3]`, and world impulse `[f32;3]` |
| 6 | Bullet impacts | `count:u32`, then 20-byte rows: source tick `i32`, shooter player `u8` (`255` unknown), flags `u8` (zero in schema 1), reserved `u16`, exact world position `[f32;3]` |
| 7 | Grenade detonations | `count:u32`, then 24-byte rows: source tick `i32`, grenade type `u8`, flags `u8` (bit 0 = entity known), reserved `u16`, entity id `u32`, exact world position `[f32;3]` |
| 8 | Fire-bullets inputs | `count:u32`, then 152-byte rows preserving raw `CMsgTEFireBullets` ray inputs and source order; this section never contains a fabricated endpoint |
| 9 | Player states | v13: `count:u32`, then 32-byte state-change rows described below |
| 13 | World entities | `count:u32`, then 56-byte door/breakable/mover rows described below |
| 14 | Death-input provenance | `count:u32`, then 24-byte rows: kill-event tick `i32`, selected dead-row source tick `i32`, victim player `u8`, origin-present flags `u8` (bit 0), reserved `u16`, nullable server origin `[f32;3]` (zero-filled when absent) |

Tag 14 rows use the same earliest dead-row selection as tag 5: victim identity
must resolve, the row must be dead, and its source tick must fall from the kill
event tick through two ticks later with at least one ragdoll damage field
present. The server origin may still be null on that selected row. These are
parser observations without per-property update witnesses; a present value can
be inherited network state. Tag 14 is diagnostic provenance and is not a
physics seed.

### World entities (S2EX tag 13, schema 1)

Doors, breakables, movers and props the map spawns, and what happened to them during the round.
Written only by a parser advertising `s2r-world-entities-v1`; older files simply have no tag 13,
and a reader treats its absence as an empty lane rather than as missing data.

```text
+0   tick:i32
+4   entity_id:u32
+8   serial:u32
+12  class_id:u8        1 door_rotating, 2 breakable, 3 dynamic_prop, 4 physics_prop,
                        5 func_brush, 6 func_water, 7 button
+13  operation:u8       0 spawn, 1 update, 2 delete
+14  flags:u16          bit0 origin, bit1 angles, bit2 door_state, bit3 simulation_time,
                        bit4 model, bit5 dormant
+16  origin:[f32;3]     world position
+28  angles:[f32;3]
+40  simulation_time:f32
+44  door_state:u8      0 closed, 1 opening, 2 open, 3 closing
+45  reserved:[u8;3]    zero
+48  model:u64          m_hModel handle
```

Rows are ordered by tick. Reading rules:

- **`origin` is the map-binding key.** It is the entity's spawn position decoded from the
  networked cell coordinates, and it equals the authored origin in the map's entity lump. Match
  within about 0.1 units — network quantisation shifts it by up to 0.018 — to find the scene node
  a row drives. Every row carries it, so no row has to be resolved through its spawn.
- A `flags` bit that is clear means *unchanged*, not zero. A `spawn` row carries everything known
  at creation; an `update` row carries only what moved that tick.
- Map entities are re-created every round, so `entity_id`/`serial` identify an entity only within
  one round's window. Do not carry them across clips.
- A `delete` without the `dormant` bit is a removal: this is how a broken vent appears, since CS2
  networks no health, no broken flag and no break event. With the `dormant` bit it is only a PVS
  departure and the entity still exists.
- The writer keeps one spawn per (class, origin): a replayed full packet re-creates the same door
  under a second entity slot, and that is one door, not two. A spawn from before the window is
  kept with its tick clamped to the window start, so an entity that exists for the whole round
  can still be placed; changes from before the window are dropped.

### Networked AG2 pose recipes (S2EX tag 15, schema 1)

This sparse lane records one full recipe snapshot at each sampled tick for a player pawn
whose networked recipe fields were observed. Entity serial and per-index capture-lifetime ordinal
keep respawns and reused indices distinct. The capture ordinal is parser-local and separate from
the player `AgentLives` ordinal; actor association uses the life interval's pawn entity index and
serial. If dense player rows omit the pawn handle, the writer fills it only when exactly one player
is alive at the sample tick and the parsed segment contains exactly one AG2 `(entity index, serial)`
identity. Ambiguous associations remain unknown. It stores the networked CNmTaskSystem recipe
streams verbatim; viewer support must still evaluate the topology and payload with matching assets,
context, timing and interpolation. Unknown scalar context uses the sentinels below.

```text
+0   row_count:u32
repeat row_count:
     tick:i32
     entity_id:u32
     entity_serial:u32
     life_index:u32
     active_slot:u32             u32::MAX means unknown
     recipe_version:i32          i32::MIN means unknown
     graph_definition:u64        u64::MAX means unknown
     graph_iteration:u32         u32::MAX means unknown
     slot_count:u16
     repeat slot_count:
         topology_len:u32
         topology:[u8; topology_len]
     dynamic_len:u32
     dynamic:[u8; dynamic_len]
```

An empty topology is retained only for a slot with no topology bytes observed. The dynamic
vector is the full networked vector, including the four-byte header that CS2 skips when it
deserializes task parameters. `DEM_AnimationData` and `DEM_AnimationHeader` are separate opaque
demo commands and do not populate this lane. Record ordering is tick, entity index, serial,
then lifetime ordinal. Consumers can sample the latest snapshot at or before a tick within
the same pawn lifetime. When a new capture lifetime begins at the replay-window boundary, an
older lifetime is not seeded over it; duplicate rows for one entity index, serial and tick keep
the newer capture-lifetime ordinal.

The current demo parser does not expose an exact CS2 client patch/build number from the verified
demo header. `meta.game_version` is retained as source metadata, but consumers must treat exact
client-build compatibility as unknown unless another verified source supplies it.

Tag 9 schema 1 stores rows grouped by player-table index, then ascending tick.
Within a continuous recorded player life, unchanged samples are omitted. Read
the latest row at or before the requested tick; decrement `flash_remaining` by
elapsed ticks / 64, clamped to zero. Death rows clear blindness, and respawn or
pawn changes reset timing. Never carry a state across a missing player-frame
interval or beyond the replay range. Changes after a gap always emit a new row.

```text
+0   tick:i32
+4   player:u8
+5   flags:u8        bit0 alive, bit1 airborne, bit2 scoped, bit3 blind,
                    bit4 smoke_view_obstructed
+6   known:u8        same bits; absent means unknown
+7   eye_source:u8   0 unknown, 1 recorded view offset, 2 reconstructed
+8   flash_remaining:f32   seconds, derived from player_blind timing
+12  flash_duration:f32    raw m_flFlashDuration, not a countdown
+16  flash_max_alpha:f32   raw m_flFlashMaxAlpha, not current screen opacity
+20  eye_position:[f32;3]  world-space eye position (see eye_source)
```

Missing floats use canonical NaN (`0x7fc00000`). Flash events preceding the replay
window are retained for timing; durations without a known start remain unknown.
This models an active flash effect, not a threshold for complete visual blindness.

`smoke_view_obstructed` means visible smoke at the eye/camera position. It is
distinct from a foot inside a smoke region and from the kill's `thrusmoke` flag.
**The parser currently leaves this bit unknown:** smoke journal masks represent
collision, not rendered density. `player_states::SmokeViewObstruction` is the
integration contract for a renderer with reconstructed density and a camera
position. It is not a built-in density implementation. View offsets are optional.
When missing, eyes are reconstructed as player origin plus `64 - 18 * duck_amount`
on Z, matching S2DVR's 64-unit standing / 46-unit crouched POV convention. The raw
duck fraction handles transitions; the crouch flag supplies a fallback if the
fraction is absent. `eye_source=2` distinguishes reconstruction from recorded
offsets. Source 2 may flatten recorded view offsets to pawn `m_vecX/Y/Z`; those
are captured too. Airborne eyes follow the player origin, not the map floor. A renderer
may substitute its actual playback camera position when available.
Target-specific visibility additionally requires a camera-to-target density ray.

Unknown sentinels are `u32::MAX`, `i16::MIN`, `i8::MIN`, player index 255,
PVS 255, item id 0, and cosmetic index 0 as appropriate. Weapon identity is
the `(entity_id, entity_serial)` lifetime, not entity index alone. Inventory
slot is the `m_hMyWeapons` vector ordinal; econ inventory position is retained
separately. PVS remains a raw two-bit transition, not an asserted visibility
boolean. World velocity is explicitly flagged as derived position delta in
units per tick because no authoritative velocity netprop was observed.

Grenade detonation types reuse the existing utility enum: `0=unknown`,
`1=flashbang`, `2=HE`, `3=smoke`, `4=fire` (Molotov/incendiary), and `5=decoy`.
Unknown detonation entity IDs are `u32::MAX`. Bullet and grenade positions are
unquantized little-endian `f32` values copied directly from their game events;
events missing a complete finite XYZ are not emitted. Fire and decoy use CS2's
authoritative `inferno_startburn` and `decoy_started` source events. Same-tick
events are retained in source order and are never coalesced.

Tag 8 is a raw-input/provenance lane, distinct from exact impact tag 6. Its
schema is unchanged by the additive ground-fire extension described below.

Tag 10, schema 1, contains recorded inferno patch changes: `count:u32`, followed
by `count` fixed 44-byte rows. Offsets: tick:i32 at 0, entity:u32 at 4,
serial:u32 at 8, first-observed entity tick:i32 at 12, patch:u8 at 16,
flags:u8 at 17, inferno-type:u16 at 18, world XYZ:f32[3] at 20, and recorded
burn normal:f32[3] at 32. Flags are position-known=1, burning-known=2,
burning=4, normal-known=8, entity-removed=16. Patch indexes are 0–63; removal
uses index 255. Inferno type 65535 is unknown. A missing state is not clear.
Only changed patches are emitted. EOF is not removal. Pre-window states are
retained for mid-fire clips. Older readers skip the tag; older replay files
remain readable but require reparsing to obtain this data. Current-cache
validation requires tag 10 even when its count is zero. Capture currently
supports world-vector `m_firePositions`, not legacy integer delta arrays.

Tag 8's
schema-1 152-byte row is:

```text
+0   source_tick:i32
+4   source_order:u32
+8   presence:u32
+12  shooter_player:u8       (255 unknown)
+13  flags:u8                (bit 0 = shooter mapped)
+14  reserved:u16            (zero)
+16  player_handle:u32
+20  weapon_id:u32
+24  item_definition:u32
+28  mode:u32
+32  attack_type:u32
+36  seed:u32
+40  num_bullets_remaining:u32
+44  message_tick:i32
+48  origin:[f32;3]
+60  angles:[f32;3]
+72  entity_origin:[f32;3]
+84  inaccuracy:f32
+88  recoil_index:f32
+92  spread:f32
+96  player_inair:u8         (0/1/255 unknown)
+97  player_scoped:u8        (0/1/255 unknown)
+98  reserved:u16            (zero)
+100 extra_type:i32
+104 attack_tick_count:i32
+108 attack_tick_fraction:f32
+112 render_tick_count:i32
+116 render_tick_fraction:f32
+120 inaccuracy_move:f32
+124 inaccuracy_air:f32
+128 aim_punch:[f32;3]
+140 sound_type:i32
+144 sound_dsp_effect:u32
+148 reserved:u32            (zero)
```

Presence bits 0–25 correspond in row order to player handle, origin, angles,
entity origin, weapon id, item definition, mode, attack type, seed, inaccuracy,
recoil index, spread, bullets remaining, message tick, in-air, scoped, extra
type, attack tick/count fraction, render tick/count fraction, movement/air
inaccuracy, aim punch, sound type, and sound DSP effect. Missing unsigned values
use `u32::MAX`, signed values use `i32::MIN`, floats use canonical quiet NaN
`0x7fc00000`, and booleans use `255`. Any ray endpoint reconstructed from this
section is derived data and must retain that provenance.

Held items emit inventory membership/ammo/active changes only when those values
change. Unowned items emit world transforms only on change. This is what keeps
the authority data additive without increasing the 47-byte frame stride.

---

## Version History

| Version | Key Changes |
|---------|-------------|
| v2      | Original format. Fixed-grid frames (all ticks, all players). Player entries 44 bytes. |
| v3      | Players block gains `frame_count` (u16). Frames become alive-only with tick prefix. Player entries 46 bytes. |
| v4      | Added `traj_offset` and `wf_offset` in header. Grenade Trajectories block and Weapon Fire block introduced. Utility events `entity_id` was padding in earlier versions. |
| v5      | Angles changed from `i16` (scaled) to `f32` (degrees). Frame size increases from 35 to 39 bytes. Trajectory `entity_id` widened from `u16` to `u32`. |
| v6      | Appends normalized weapon/glove cosmetic indexes (`u32` each) to every alive frame. The signature tables are in `meta.cosmetics`; frame size is 47 bytes. |
| v7      | Uses reserved header bytes 46–49 for `audio_offset`; appends the length-delimited raw-audio block. The v5/v6 frame layout and all existing offsets remain unchanged. |
| v8      | Uses header bytes 50–53 for `smoke_offset`; appends exact networked smoke-voxel entity lifetimes and cumulative payload frames. |
| v9      | Uses bytes 54–61 for a bounded `S2EX` tail containing sparse agent-life, weapon-lifetime, all-player inventory, authoritative world-weapon, fatal ragdoll-impact, ordinary bullet-impact, grenade-detonation, and raw fire-bullets-input sections. Hot frames remain 47 bytes. |
| v10     | Keeps the v9 byte layout and marks replay assets regenerated with corrected smoke-voxel capture semantics. |
| v11     | Keeps the v10 byte layout and stores collection killer/victim metadata as deterministic S2R player-table indexes rather than parser entity indexes. |
| v12     | Adds shared round replay scope and round/source metadata. |
| v13     | Completes kill flags, adds known masks and penetration counts in former padding, and adds sparse timed player states in S2EX tag 9. |
| v14     | Adds input-mask observation/source flags without changing the frame layout. |
| v15     | Adds utility authority in S2EX tag 12/schema 1 and retains pre-window raw audio history. |
| v16     | Dictionary-codes utility authority in S2EX tag 12/schema 2. |
| v17     | Adds diagnostic death-input provenance in S2EX tag 14/schema 1. |
| v18     | Adds opaque networked AG2 recipe snapshots in S2EX tag 15/schema 1. |

---

## How It Is Used in This Project

### Writing (Parser → S2R)

The tick-by-tick parser (`collection_processor.rs`) calls `write_collection_s2r()` after completing a two-pass demo parse:

1. **Pass 1 (player data):** Collects per-tick position, angle, health, ammo, velocity, and flag state for each player into `grouped_records: HashMap<u64, Vec<TickRecord>>`.
2. **Pass 2 (grenade/event data):** Collects utility throw events, grenade trajectory points, weapon fire events, authoritative raw audio messages, and smoke entity network-vector updates.
3. Audio and smoke lifetimes are filtered to each collection's inclusive replay range; smoke lifetime prefixes are retained for occupancy reconstruction.
4. The writer serialises all collected data into the `.s2r` binary, computing section offsets before writing so the header can be written first.

Output files are named using the same convention as NPZ files but with a `.s2r` extension, e.g.:

```
ACE_1-6497baae-ef12-4d78-95b9-f6fd432c9131_4_s2r
```

### Reading (CollectionBrowser)

`s2r_loader.rs` implements `NpzData::load_from_s2r_file()` which reads an `.s2r` file and produces the same `NpzData` struct that the radar view uses — making `.s2r` a drop-in alternative to `.npz` with no external dependencies.

The loader is version-aware for legacy replay data through v6. The S2DVR C# reader
handles v7 audio and v8 smoke data; other readers may skip the appended blocks by
their header offsets:
- v2 uses fixed-grid frame parsing
- v3/v4 use variable-length alive-only frames with i16 angles
- v5 uses variable-length alive-only frames with f32 angles
- v6 uses v5 frames plus cosmetic-index extensions

### Inspection / Debugging

`s2r_inspector.py` and `s2r_diag.py` provide drag-and-drop tools to parse and human-read `.s2r` files:

```
python s2r_inspector.py <file.s2r>
```

This produces a `.txt` dump alongside the `.s2r` file showing the header, player list, kill events, utility events, trajectories, weapon fire, and per-player tick tables — useful for verifying parser output.

### Cross-language fixtures

The external consumer regression fixtures and generator binaries are not included in this
source-only distribution. The historical v6 fixture is a frozen compatibility oracle; it
should not be overwritten by the current writer.

---

## Relationship to NPZ

Older parser revisions could emit `.npz` files carrying a subset of the replay data. That format is retained only as historical context and is not a production output.

| Feature           | `.npz`                        | `.s2r`                         |
|-------------------|-------------------------------|-------------------------------|
| Encoding          | NumPy compressed arrays       | Custom flat binary             |
| Dependencies      | NumPy (Python)                | None (pure binary)             |
| Seekability       | Requires full decompression   | Random access via header offsets |
| Angle precision   | f64 (double)                  | f32 (v5+) / scaled i16 (v3/v4) |
| Primary use       | Python analytics / tooling    | CollectionBrowser / s&box editor |




## v14 input observation/source contract

The 47-byte hot frame layout stays unchanged. Existing flags10/12 retain primary/secondary
values. Flag13 means the selected input mask was observed; flag14 selects decoded
UserCommandState1 rather than MovementPrevious. Flag15 is reserved. For versions below14,
input provenance is always Unknown regardless of high flag bits. In v14 flag13 clear means
Unknown regardless of flag14; 13set/14clear means MovementPrevious; bothset means UserCommandState1.
No S2EX tag is added or reused: tag9 PLAYER_STATES is unchanged. The raw64bit optional mask
is internal only. Capability s2r-input-mask-source-v1 advertises producer support, not the
provenance of an old file. Current-format cache validation regenerates pre-v14 assets only
on a later requested parse; older files stay readable with unknown input provenance.

The first usable source, including Some(0), wins. Missing/null/out-of-range/invalid masks
are unknown; floating and negative coercions are rejected. Post-merge optional usercmd
button fields preserve Some(0) and clear stale properties for None; known inherited state
remains decoded-state provenance. Neither source certifies native/current-command/subtick
timing. MovementPrevious onset may only be labeled reconstructed presentation; no fixed
tick correction is invented. Consumers mark only exact active sparse frame observations;
filled/synthetic ticks are unknown and source/life/weapon changes or gaps break epochs.
