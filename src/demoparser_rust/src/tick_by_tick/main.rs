// Import our organized modules
mod button_press;
mod cache;
mod cli;
mod collection_processor;
mod config_manager;
mod csv_output;
mod data_extraction;
mod data_processing;
mod data_types;
mod demo_loader;
mod input_discovery;
mod kill_collection_parser;
mod npz_output;
mod parser_config;
mod team_parser;
mod velocity_processing;
mod weapon_fire_detector;
mod weapon_inspect;
mod weapon_mapper;

use cli::parse_cli_args;
use collection_processor::CollectionProcessor;
use config_manager::{load_config, merge_cli_overrides};
use input_discovery::{discover_collection_files, validate_collection_files};
use kill_collection_parser::parse_kill_collection_csv;
use std::process;
use std::time::Instant;

fn main() {
    // Parse command line arguments
    let cli_args = parse_cli_args();

    // Check if input was provided via drag-and-drop
    let _input_source = if std::env::args().len() > 1 && !cli_args.input.starts_with("--") {
        "drag-and-drop or command line"
    } else {
        "command line"
    };

    // Load configuration
    let config = match load_config(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading configuration: {}", e);
            process::exit(1);
        }
    };

    // Merge CLI overrides with configuration
    let final_config = merge_cli_overrides(config, &cli_args);

    println!("Starting Tick-By-Tick Processing");

    // Discover input files
    let collection_files = match discover_collection_files(&cli_args.input) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("Error discovering input files: {}", e);
            process::exit(1);
        }
    };

    // Validate collection files
    if let Err(e) = validate_collection_files(&collection_files) {
        eprintln!("Error validating collection files: {}", e);
        process::exit(1);
    }

    // Start timing
    let start_time = Instant::now();

    // Initialize processor
    let processor = CollectionProcessor::new(final_config, cli_args.clone());

    // Process each collection file
    let mut total_errors = 0;
    let mut collection_counts = std::collections::HashMap::new();

    println!("Batch job 1 of 1");
    for collection_file in &collection_files {
        // Parse kill collection CSV
        let collection_data = match parse_kill_collection_csv(collection_file) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("Error parsing collection file {}: {}", collection_file.display(), e);
                total_errors += 1;
                continue;
            }
        };

        // Process collections
        match processor.process_collections(&collection_data) {
            Ok(results) => {
                for result in results {
                    *collection_counts.entry(result.collection_type).or_insert(0) += 1;
                }
                if let Some(file_name) = collection_file.file_name().and_then(|n| n.to_str()) {
                    println!("Processed {}", file_name);
                }
            }
            Err(e) => {
                eprintln!("  ✗ Error processing collections: {}", e);
                total_errors += 1;
            }
        }
    }

    // Calculate elapsed time
    let elapsed = start_time.elapsed();

    // Print summary
    println!();
    println!("Batch job finished in {:.2} seconds", elapsed.as_secs_f64());
    println!("Processed {} ACEs, {} QUADs, {} TRIPLES, {} MULTIs, {} SINGLEs",
        collection_counts.get("ACE").unwrap_or(&0),
        collection_counts.get("QUAD").unwrap_or(&0),
        collection_counts.get("TRIPLE").unwrap_or(&0),
        collection_counts.get("MULTI").unwrap_or(&0),
        collection_counts.get("SINGLE").unwrap_or(&0)
    );
    if total_errors > 0 {
        println!("Processing completed with {} error(s)", total_errors);
        process::exit(1);
    }

    // Auto-close if requested
    if cli_args.autoclose {
        println!("Auto-closing application...");
    } else {
        println!("Press Enter to exit...");
        let mut input = String::new();
        let _ = std::io::stdin().read_line(&mut input);
    }
}
