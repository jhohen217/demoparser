//! Main module for the KillCollectionWriter crate
//!
//! This module contains the main function for the KillCollectionWriter crate.

use std::env;
use std::path::{Path, PathBuf};
use std::process;

use kill_collection_writer::writers::collection_writer::CollectionWriter;
// Note: The master CSV functionality is handled separately in the KillCollectionMaster binary

/// Load configuration to get ParserOutput path
fn load_config() -> Result<String, Box<dyn std::error::Error>> {
    // Try to find config.ini file
    let config_path = find_config_file()?;
    let content = std::fs::read_to_string(&config_path)?;

    // Parse INI content to find ParserOutput
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("ParserOutput") && line.contains('=') {
            let value = line.split('=').nth(1).unwrap().trim();
            return Ok(value.to_string());
        }
    }

    Err("ParserOutput not found in config".into())
}

/// Find the config.ini file
fn find_config_file() -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Try executable directory first
    if let Ok(exe_path) = env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let config_path = exe_dir.join("config.ini");
            if config_path.exists() {
                return Ok(config_path);
            }
        }
    }

    // Fallback to current working directory
    let cwd_config = PathBuf::from("config.ini");
    if cwd_config.exists() {
        Ok(cwd_config)
    } else {
        Err("config.ini not found".into())
    }
}

/// Main function
fn main() {
    // Get the command line arguments
    let args: Vec<String> = env::args().collect();

    // Check if a demo path was provided
    if args.len() < 2 {
        eprintln!("Usage: {} <demo_path>", args[0]);
        process::exit(1);
    }

    // Get the demo path
    let demo_path = &args[1];

    // Load configuration to get ParserOutput path
    let parser_output = match load_config() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("Error loading config: {}", e);
            process::exit(1);
        }
    };

    // Extract folder from demo path
    let folder = interface::utils::parser_utils::get_folder_from_demo_path(demo_path);

    // Generate output path: ParserOutput/KillCollections/{folder}/{demo_name}_col.csv
    let demo_name = Path::new(demo_path).file_stem().unwrap().to_str().unwrap();
    let output_dir = PathBuf::from(parser_output)
        .join("KillCollections")
        .join(&folder);
    let output_path = output_dir.join(format!("{}_col.csv", demo_name));

    // Create a collection writer
    let mut writer = match CollectionWriter::new(demo_path, output_path.to_str().unwrap()) {
        Ok(writer) => writer,
        Err(e) => {
            eprintln!("Error creating collection writer: {}", e);
            process::exit(1);
        }
    };

    // Use the interface module's process_demo function to get collections
    let collections = match interface::process_demo(demo_path) {
        Ok(collections) => collections,
        Err(e) => {
            eprintln!("Error processing demo: {}", e);
            process::exit(1);
        }
    };

    // Write all sections
    let death_events = writer.get_death_events();
    match writer.write(&collections, &death_events) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error writing collections: {}", e);
            process::exit(1);
        }
    }

    // Close the writer
    match writer.close() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error closing writer: {}", e);
            process::exit(1);
        }
    }

    // Note: Master CSV updates are handled by the separate KillCollectionMaster binary
}
