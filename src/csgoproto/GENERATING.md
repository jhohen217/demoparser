# Generated parser inputs

Normal builds consume `src/protobuf.rs`, `src/message_type.rs` and `src/maps.rs`.
They never download schemas, run nested Cargo, or regenerate source. Missing
generated inputs produce an error instead of an implicit download.

Regeneration is a separate developer action that intentionally rewrites those files.
Use a locally prepared, reviewed `GameTracking-CS2` checkout and a local `protoc`.
From this crate's directory in PowerShell:

```powershell
$env:CSGOPROTO_REGENERATE = '1'
try { cargo build --locked --offline --lib; if ($LASTEXITCODE) { throw 'Protocol generation failed' } }
finally { Remove-Item Env:CSGOPROTO_REGENERATE }
cargo run --locked --offline --bin csgoproto
```

The first command regenerates protobuf bindings. The explicit binary run regenerates
message IDs and item maps from the local schema and game-data files. Review all
generated diffs together before using them. Normal builds do not run either action.
