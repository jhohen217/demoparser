# CS2 demo parser and writer essentials

This fork is based on [LaihoE/demoparser](https://github.com/LaihoE/demoparser),
with a parser CLI, demo writer, entity-editing helpers, and an experimental static-light injector.
The unfinished demo upgrader is excluded.

## Build

Install stable Rust and your platform's C/C++ build tools. On Windows, use Visual Studio Build
Tools with Desktop development with C++. Python 3.11+ runs the build wrapper. Cargo downloads
third-party dependencies on the first build. Normal builds consume checked-in generated protobuf
bindings and do not require protoc or the CS2 installation.

```powershell
python scripts/build.py
```

Executables and a portable example configuration are placed in `out/`. To reuse a build cache,
pass `--target-dir <directory>`. An existing separate parser cache can be passed with
`--parser-target-dir <directory>`. Use `--offline` only after dependencies have been downloaded.
Rust parser dependencies are compiled into the writer; users do not install another parser
executable or Python package to use it. `Demoparser` includes the main writer commands too.

## Features and limits

| Tool | Scope |
| --- | --- |
| `demo-writer` / `Demoparser trim` | Single round, contiguous round range, or separate batch round clips; raw, gzip and Zstandard inputs; tick rebasing and output checks |
| `demo-writer retarget` / `Demoparser retarget` | Official or Workshop map identity/world changes; player coordinates stay in the original map space |
| `demo-writer suppress` | Shot-message, sound and guarded entity-state editing; cosmetic, aim and animation options are advanced features with separate playback limitations |
| Writer diagnostics | Frames, schema fields, rounds, input subticks, weapon timelines, recipes, events, string tables, packet round-trip checks and player properties |
| `schedule-entity-fields` | Exact tick/entity/class/serial guarded scalar writes; edits existing entities and creates, not a general entity spawner |
| `checkpoint-insert-entities` | Restores entities already present in the recorded sequential stream into seek checkpoints |
| `audit-entity-fields`, `inspect-entity-events` | Read-only lifetime-aware field and create/delete inspection |
| `Demoparser` | Collection catalogs, S2R replay data, player/weapon/cosmetic/utility data, recorded smoke/sound/AG2 payloads, FACEIT fetch and integrated trimming |
| `CollectionBrowser` | Optional existing collection GUI; build separately with `--browser` |
| `light-demo-probe` | Experimental static omni/barn/rect light transplant from a same-schema donor into startup and seek checkpoints |

The light extension had successful native Nuke playback and bounded seek tests in September 2026.
The adapted release source requires fresh native validation on the intended client and fixtures.
It is not a general light editor: donor and target schemas must match, entity index 2047 must be
unused and above source records, and existing light-class baselines are rejected. One static light
is retained for the full demo. Movement, timed lifetimes, parenting, cookies and shadows are not
qualified release features. CPU tests and structural validity do not establish native playback.

Shot suppression cannot revive victims or regenerate missing simulation. Damage/death edits are
refused by default unless explicitly overridden. Do not promise that every decal, animation or
viewer prediction mode is fully removed. Cosmetic and animation options need validation on each
intended schema/asset combination; changing a weapon definition alone can crash playback.

Smoke, audio and AG2 extraction preserve recorded inputs; they do not themselves reconstruct
visible smoke, playable audio or complete native poses. S2R is a replay/export container, distinct
from an edited `.dem`. See `src/demoparser_rust/docs/S2R_FORMAT.md` for the format.

## Examples

```text
demo-writer retarget match.dem --map de_nuke --keep-manifests --sky maps/prefabs/de_nuke/de_nuke_skybox02 --output swapped.dem
demo-writer trim match.dem --round 8 --output round8.dem
demo-writer inspect round8.dem
demo-writer verify round8.dem
demo-writer suppress match.dem --list
Demoparser --capabilities
Demoparser fetch --help
light-demo-probe snapshot donor.dem 363 2900 light.json
light-demo-probe inject target.dem donor.dem light.json edited.dem
```

For Workshop retargeting supply `--addon <published-file-id>` and the item's actual internal
world name to `--map`; supply its actual prefab path to `--sky` when needed. The ID selects the
Workshop item's current version, not a historical map version. Keep manifests by default in
examples to retain shared rendering resources. Different map geometry can misalign gameplay.

Outputs should use new filenames. Keep originals. Entity schedules resolve field paths from the
source class serializer; field numbers and resource handles are build-specific. Run each helper
with no arguments for its usage (the legacy entity helpers report usage as an error).

## Verification and publishing

Build from this source tree, then run `cargo test --locked --all-targets` in `DemoWriter`. Parser tests that need
the upstream large `test_demo.dem` fixture are separate from fixture-free tests. Do not commit
test demos, binaries, output data, local credentials, captures or build directories. The preparation
report records exact tested targets and any skipped fixture-dependent/native checks.

The initial preparation input hashes are retained in
`preparation-source-provenance.json`. `PUBLICATION-MANIFEST.json` records the essential release
source after merging current upstream fixes. The inherited upstream bindings remain in the fork;
the bundled Windows release contains the Rust CLI tools. See `PUBLICATION-NOTES.md` for validation.
