# Windows tools

Extract the complete Windows ZIP into a writable folder and open PowerShell there.
These are x64 executables. The Microsoft C/C++ runtime is a runtime requirement;
it is not included in the ZIP. Python and a separate demoparser installation are not
needed to run the compiled tools. CS2 is needed to test native demo playback.

`config.ini` is a portable example with relative output paths and no credentials.
Edit it for collection parsing, and keep populated FACEIT credentials local.

```powershell
.\Demoparser.exe --capabilities
.\Demoparser.exe retarget --help
.\demo-writer.exe retarget match.dem --map de_nuke --keep-manifests --sky maps/prefabs/de_nuke/de_nuke_skybox02 --output swapped.dem
.\demo-writer.exe trim match.dem --round 8 --output round8.dem
.\demo-writer.exe verify round8.dem
.\light-demo-probe.exe --help
```

See `README.md` for the full feature table, including limitations of entity and animation
edits, and `docs/light-validation.md` for the experimental light tool's validation scope.
Workshop retargeting requires the item's published-file ID, actual internal world name
and appropriate sky prefab. A retarget changes map identity; it does not reposition gameplay.

`Demoparser` also dispatches the writer commands, so the standalone `demo-writer` is optional
when sharing only map retargeting and trimming. The entity and light helper executables
provide their separate advanced operations. The optional collection GUI is source-only.

Keep original demos and use new output names. Structural checks and parser checks establish
different levels of validity; neither substitutes for playback on the intended CS2 client.

## Building from source

Use Rust stable, Python 3 and Visual Studio 2022 Build Tools with the C++ workload
and Windows SDK. Run `python scripts/build.py` from the repository root. The checked-in
Cargo configuration supplies `/std:c++17 /EHsc` for Windows x64 C++ dependencies.
Keep the `.cargo` directory when copying the source.

CI uses `windows-2022` because the pinned DuckDB dependency includes an older fmt
implementation that references `stdext::checked_array_iterator`. MSVC 14.51 removed
that type; see [Microsoft's compatibility patch](https://github.com/microsoft/PowerToys/blob/main/deps/vcpkg-overlays/spdlog/msvc-14.51-stdext-checked-array-iterator.patch).
