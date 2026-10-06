# Initial parser/writer release

Built on official upstream commit `45ca85aeac8fb0de9d385124d2c7fe0e7b8ff0c8`.
New upstream schema, entity-handle, button-mask, repeated-input and velocity-history fixes
were merged with the parser/writer extensions. Upstream Python/JavaScript sources were kept;
Python sticker rows follow the new explicit slot/offset/scale/rotation schema, and the Node
manifest no longer requests a nonexistent voice feature. Both bindings passed compile checks.
The upstream PyPI deployment workflow remains restricted to the upstream repository.

All seven Windows tools built successfully using the pinned release dependencies.
357 Rust tests passed: writer 80, entity helpers 2, parser 75, application 169 and downloader 31.
This includes both formerly ignored real-demo tests. The complete optional upstream large-fixture
suite was not run. Tests also checked retarget output safety and malformed light snapshots.

Real-demo smoke checks passed for packet round trips, default-map and Workshop-ID metadata
retargeting, round trimming/reparse, entity writes and lifetime rejection, and light transplantation.
The injected omni fixture was byte-identical to the earlier native-tested output; 500 audited
original pawn fields stayed unchanged. This run did not launch CS2 or capture native playback.
Nuke Night's current world/sky assets and client playback have not been verified.

Static lights remain experimental, with same-schema donor and reserved-index/baseline constraints.
There are no general animated lights, arbitrary lifetimes or shadow guarantees. Recorded smoke,
audio and AG2 exports preserve inputs rather than independently reconstructing native effects.
Live FACEIT credentials/network service access was not exercised; downloader tests used local mocks.
The unfinished demo upgrader is excluded.

The compact source asset excludes demos, captures, game assets, binaries, build caches and
credentials. The GitHub fork retains inherited upstream sources/history and test fixtures.
Use the manually attached source ZIP when a minimal source package is wanted; GitHub's automatic
whole-repository archives include the inherited upstream fixtures.

`PUBLICATION-MANIFEST.json` records the essential source files. The separate preparation manifest
is historical and documents the input to fork preparation. GitHub Actions results are available
on the repository; they are separate from the local build/test results above.
