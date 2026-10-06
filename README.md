# CS2 demo parser and writer

An upstream-based fork of [LaihoE/demoparser](https://github.com/LaihoE/demoparser) with tools for editing and exporting CS2 demos.

- Retarget demos to default or Workshop maps.
- Trim individual rounds, ranges, or batches from raw, gzip, and Zstandard demos.
- Apply guarded entity-field edits and repair seek checkpoints.
- Suppress shots and inspect demo packets, inputs, events, and animation data.
- Export collections and S2R replay data, including recorded smoke, audio, and AG2 inputs.
- Download and process FACEIT demos.
- Transplant experimental static lights from a same-schema donor.

[Download Windows tools](https://github.com/jhohen217/demoparser/releases/tag/writer-v0.1.0) · [Features and limits](docs/FEATURES.md) · [Usage](RUNNING.md)

Build the Rust tools with `python scripts/build.py` (Rust and C/C++ build tools required).
The compiled tools include their parser dependency; no separate Python package is needed.

Map swaps retain original gameplay coordinates. Static lights remain experimental and require matching schemas. The unfinished demo upgrader is excluded.

The official repository remains the upstream source for future parser updates. Its Python/JavaScript sources and documentation are retained; this fork's Windows release focuses on the Rust tools.
