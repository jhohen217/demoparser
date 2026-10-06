use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Get the output directory from cargo
    let out_dir = env::var("OUT_DIR").unwrap();
    let _profile = env::var("PROFILE").unwrap();

    // Determine the target directory based on the profile
    let target_dir = Path::new(&out_dir)
        .ancestors()
        .nth(3) // Go up three levels from OUT_DIR to reach target/<profile>
        .unwrap();

    // Source config.ini path (relative to the project root)
    let source_config = Path::new("../../config.ini");

    // Destination config.ini path in the target directory
    let dest_config = target_dir.join("config.ini");

    // Copy the config.ini file to the target directory
    println!("cargo:rerun-if-changed=../../config.ini");

    if source_config.exists() {
        match fs::copy(source_config, &dest_config) {
            Ok(_) => println!("Copied config.ini to {}", dest_config.display()),
            Err(e) => eprintln!("Failed to copy config.ini: {}", e),
        }
    } else {
        eprintln!("Source config.ini not found at {}", source_config.display());
    }
}
