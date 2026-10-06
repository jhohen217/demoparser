# Static light validation history

The original research prototype was tested in native CS2 on de_nuke on September 20, 2026.
It transplanted a static `COmniLight`, `CBarnLight` or `CRectLight` from recorded same-schema
donors into a target demo. Localized wall, ground and player illumination was visible.
The omni light survived full reload and forward/backward seeks through both checkpoints.
Full reload and checkpoint coverage were not repeated for every light shape.

The source and target required byte-identical decoded class information and send tables.
The original omni test scanned 4,394 entity packets, whose highest record index was 511,
and inserted creates at reserved index 2047 at initial packets and checkpoints (ticks 1 and 3841).
The original pawn at tick 1796 retained all 500 decoded fields and its baseline.

The original omni output SHA-256 was
`2cdb3f89504968218672e042914c11a7e3827fb580e5cb34910bd2aeed1607e0`.
This is a historical reference, not evidence that a new build or different demo is valid.

Dark-interior follow-up showed an angularly restricted omni light, barn-door beam and dedicated
rectangular light. Shadows and bounce lighting were disabled. Display-RGB comparisons showed
localized changes; exposure was not locked, so the measurements were not photometric calibration.
HLAE was loaded for camera control and capture. Playback without HLAE was not established.

The integrated publication candidate builds directly with the writer library. It also binds
each snapshot to its donor SHA-256 and validates snapshot operands before rewriting.
Regenerate snapshots with `light-demo-probe snapshot`; legacy snapshots lack the donor binding.
Run `light-demo-probe --help` for the current interface and use new output filenames.

Fresh native playback on the intended client remains required before presenting this as a
supported general feature. Arbitrary maps/schemas, animated lights, lifetimes, parenting,
cookies, dynamic shadows and reconciliation of existing light baselines remain unverified.
No demo fixtures, game assets, native hooks or capture data are included in this source package.
