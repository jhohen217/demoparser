# Upstream updates

This repository is a fork of `LaihoE/demoparser`. The `main` branch contains the parser/writer
extensions on top of official upstream history. Keep your fork as `origin` and the official
repository as `upstream`.

Fetch upstream updates into a feature branch, review conflicts in shared parser/protobuf code,
run the parser/writer tests and rebuild, then merge into main. Do not overwrite the writer or
import workstation histories, binaries, populated credentials, demo fixtures or captures.
Publish compact Windows builds as GitHub release assets.

The initial preparation hashes are historical; the publication manifest describes the essential
release source. Future updates require a new build and validation record.
