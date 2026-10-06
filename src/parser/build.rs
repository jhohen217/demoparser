use std::{io, path::Path};

fn main() -> io::Result<()> {
    // Generated inputs belong to csgoproto. Building a consumer must never
    // launch nested Cargo or rewrite another crate's source tree.
    for name in ["protobuf.rs", "message_type.rs", "maps.rs"] {
        let path = format!("../csgoproto/src/{name}");
        println!("cargo::rerun-if-changed={path}");
        if !Path::new(&path).is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound,
                format!("Missing generated input {path}; see ../csgoproto/GENERATING.md")));
        }
    }
    Ok(())
}
