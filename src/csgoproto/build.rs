use std::{io::{self, Result}, path::Path};

fn main() -> Result<()> {
    println!("cargo::rerun-if-env-changed=CSGOPROTO_REGENERATE");
    if std::env::var("CSGOPROTO_REGENERATE").as_deref() != Ok("1") {
        for name in ["protobuf.rs", "message_type.rs", "maps.rs"] {
            let path = format!("src/{name}");
            println!("cargo::rerun-if-changed={path}");
            if !Path::new(&path).is_file() {
                return Err(io::Error::new(io::ErrorKind::NotFound,
                    format!("Missing generated input {path}; see GENERATING.md")));
            }
        }
        return Ok(());
    }

    let protos = vec![
        "GameTracking-CS2/Protobufs/steammessages.proto",
        "GameTracking-CS2/Protobufs/gcsdk_gcmessages.proto",
        "GameTracking-CS2/Protobufs/demo.proto",
        "GameTracking-CS2/Protobufs/cstrike15_gcmessages.proto",
        "GameTracking-CS2/Protobufs/cstrike15_usermessages.proto",
        "GameTracking-CS2/Protobufs/usermessages.proto",
        "GameTracking-CS2/Protobufs/networkbasetypes.proto",
        "GameTracking-CS2/Protobufs/engine_gcmessages.proto",
        "GameTracking-CS2/Protobufs/netmessages.proto",
        "GameTracking-CS2/Protobufs/network_connection.proto",
        "GameTracking-CS2/Protobufs/cs_usercmd.proto",
        "GameTracking-CS2/Protobufs/usercmd.proto",
        "GameTracking-CS2/Protobufs/gameevents.proto",
        "GameTracking-CS2/Protobufs/cs_gameevents.proto",
    ];

    for proto in &protos {
        println!("cargo::rerun-if-changed={proto}");
        if !Path::new(proto).is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound,
                format!("Missing local schema {proto}; regeneration never downloads inputs")));
        }
    }

    prost_build::Config::new()
        .format(false)
        .out_dir("src")
        .default_package_filename("protobuf")
        .bytes(["."])
        .enum_attribute(".", "#[derive(::strum::EnumIter)]")
        .compile_protos(&protos, &["GameTracking-CS2/Protobufs/"])
}
