//! Main binary for the demoparser_rust project
//!
//! This binary provides a command-line interface for processing CS2 demo files
//! and generating master files (DuckDB) and tick-by-tick data (NPZ/DuckDB).

use std::collections::HashMap;
use std::env;
use std::io::IsTerminal;
use std::process;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use config::AppConfig;
use demoparser::{run_parsing, utils, InputKind, ProgressEvent};

mod catalog_repair;
mod progress_json;

// Replay workers share large entity snapshots; avoid contention on the Windows process heap.
#[cfg(windows)]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.get(1).is_some_and(|command| command == "fetch") {
        return run_fetch_cli(&args).await;
    }

    // Demo rewriting is exposed by this executable but implemented in the focused DemoWriter
    // crate. Dispatch before the legacy permissive argument reader, which would otherwise treat
    // `trim` and its output path as ordinary parser inputs.
    if args
        .get(1)
        .is_some_and(|command| matches!(command.as_str(), "inspect" | "net-ticks" | "rounds" | "overlay" | "inputs" | "verify" | "serverinfo" | "retarget" | "suppress" | "fields" | "class-paths" | "pose" | "weapon-timeline" | "decals" | "events" | "roundtrip" | "tables" | "props" | "trim"))
    {
        return demo_writer::run_from(std::env::args_os().collect());
    }
    if args
        .get(1)
        .is_some_and(|command| command == "repair-catalog")
    {
        return catalog_repair::run_from(&args[2..]);
    }

    // Parse command line arguments
    let (arg_map, input_paths) = utils::parse_arguments(&args);

    // Answered before anything else, including the input-path check: a host application asks what
    // this build supports before it has decided what to run. The generic argument reader accepts
    // unknown switches silently, so without something to ask, a host cannot tell a parser that
    // honours --select from an older one that ignores it and parses everything.
    if arg_map.contains_key("capabilities") {
        println!("{}", progress_json::capabilities());
        return Ok(());
    }

    // Check for required input paths
    if input_paths.is_empty() {
        utils::print_usage(&args[0]);
        process::exit(1);
    }

    println!("Processing {} input paths...", input_paths.len());

    // Load configuration
    let mut config = AppConfig::load()?;

    // Apply command line arguments to override config
    if let Some(padding) = arg_map.get("padding") {
        if let Ok(padding_value) = padding.parse::<i32>() {
            println!("Using padding value from command line: {}", padding_value);
            config.parser.pad_ticks = padding_value;
        } else if padding == "all" {
            println!("Using full round padding (from freeze time end to round end)");
            config.parser.pad_ticks = 0;
        }
    }
    if arg_map.contains_key("overwrite") {
        config.parser.overwrite = true;
        println!("Overwrite mode enabled from command line.");
    }
    if arg_map.contains_key("trim-collections") {
        config.parser.trim_collection_rounds = true;
        println!("Verified round trimming enabled from command line.");
    }
    if arg_map.contains_key("delete-source-after-trim") {
        if !config.parser.trim_collection_rounds {
            anyhow::bail!("--delete-source-after-trim requires --trim-collections");
        }
        config.parser.delete_source_after_trim = true;
        println!(
            "Original DEMs and compressed archives will be deleted after verified clips and requested S2R/DuckDB commits; published round clips are kept."
        );
    }
    let asset_refresh_only = arg_map.contains_key("asset-refresh-only");
    if asset_refresh_only {
        config.parser.trim_collection_rounds = false;
        config.parser.delete_source_after_trim = false;
        println!("Selected replay-asset refresh enabled; clip-local timing is synchronized and annotations are preserved.");
    }
    // Machine-readable progress, for a host application driving this as a child process. Off by
    // default: the prose below is what a person at a console wants, and nothing that runs this
    // today should change behaviour.
    let ndjson = arg_map.contains_key("ndjson");

    let input_kind = match arg_map.get("source-kind").map(String::as_str) {
        None | Some("all") => InputKind::All,
        Some("raw") => InputKind::Raw,
        Some("archives") => InputKind::Archives,
        Some(value) => anyhow::bail!(
            "--source-kind must be one of: all, raw, archives (received {value})"
        ),
    };

    // Re-parsing a chosen set of collections rather than everything in the demos. Passed as a file
    // rather than on the command line because a selection is routinely hundreds of entries, which
    // is past what a command line will carry.
    let collection_filter = match arg_map.get("select") {
        Some(path) => Some(load_collection_selection(path)?),
        None => None,
    };

    if asset_refresh_only && collection_filter.is_none() {
        anyhow::bail!("--asset-refresh-only requires --select <selection.json>");
    }

    if let Some(filter) = &collection_filter {
        println!(
            "Parsing a selection: {} collections across {} demos",
            filter.values().map(|v| v.len()).sum::<usize>(),
            filter.len()
        );
    }

    let thread_count = config.get_optimal_threads();
    let parser_output = &config.paths.parser_output;

    println!("Using {} threads for parallel processing", thread_count);
    println!("Output directory: {}", parser_output.display());

    // Progress bar management
    let progress_bar = Arc::new(Mutex::new(None::<ProgressBar>));
    let pb_clone = progress_bar.clone();

    // Stats tracking for final print
    let stats = Arc::new(Mutex::new(HashMap::new()));

    let stats_clone = stats.clone();

    let (batch_results, tbt_results) = run_parsing(
        input_paths,
        config.clone(),
        move |event| {
            if ndjson {
                progress_json::emit(&event);

                // The summary printed after parsing reads these, so they are still recorded.
                if let ProgressEvent::Finished {
                    total_successful,
                    total_failed,
                    ..
                } = &event
                {
                    let mut s = stats_clone.lock().unwrap();
                    s.insert("successful", *total_successful);
                    s.insert("failed", *total_failed);
                }

                return;
            }

            match event {
                ProgressEvent::PipelineMetrics { completed_demos, skipped_demos, elapsed, final_sample, .. } => {
                    let label = if final_sample { "Full pipeline" } else { "Pipeline so far" };
                    if let Some(rate) = demoparser::pipeline_metrics::seconds_per_demo(elapsed, completed_demos) {
                        println!("{}: {:.3} seconds/demo ({} completed, {} skipped)", label, rate, completed_demos, skipped_demos);
                    } else {
                        println!("{}: n/a seconds/demo (no demos completed, {} skipped)", label, skipped_demos);
                    }
                },
                ProgressEvent::PhaseCompleted { .. } => {},
                ProgressEvent::ReplayProgress { filename, phase, completed, total } => {
                    println!("Replay {phase}: {filename} ({completed}/{total} collections)");
                },
                ProgressEvent::Started { total_files, total_batches: _ } => {
                    println!("Found {} demo files to process", total_files);
                    // Handle unarchived demos logic printing is inside run_parsing/helper mostly
                    // We can print more if needed
                },
                ProgressEvent::BatchStarted { batch_num, total_batches, file_count: _ } => {
                    // Initialize progress bar for batch
                     let pb = ProgressBar::new(total_batches as u64);
                     pb.set_style(
                        ProgressStyle::default_bar()
                            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>3}/{len:3} batches {msg}")
                            .unwrap()
                            .progress_chars("##-")
                    );
                    pb.set_position((batch_num - 1) as u64);
                    pb.set_message("processing");

                    if let Ok(mut lock) = pb_clone.lock() {
                        *lock = Some(pb);
                    }
                },
                ProgressEvent::BatchCompleted { batch_num, total_batches, demos_count, collections_count, elapsed } => {
                    if let Ok(lock) = pb_clone.lock() {
                         if let Some(ref pb) = *lock {
                             pb.inc(1);
                             pb.set_message(format!("batch {}/{}", batch_num, total_batches));
                             if batch_num == total_batches {
                                 pb.finish_with_message("completed");
                             }
                         }
                    }

                    println!("Batch {} completed in {:.2}s - {} demos, {} collections",
                        batch_num, elapsed, demos_count, collections_count);
                },
                ProgressEvent::DemoProcessed { filename: _ } => {
                     // Could update a spinner or sub-bar if we wanted complex UI,
                     // but CLI implementation in main.rs was simpler: just single bar for batches
                     // and printing individual checks inside batch which is hard to replicate exactly
                     // unless we pass a sub-bar.
                     // The library emits this event per demo.
                     // For now, we skip printing per-demo to avoid spamming unless we use a MultiProgress
                },
                ProgressEvent::Error { path, error } => {
                    eprintln!("Error processing {}: {}", path.display(), error);
                },
                ProgressEvent::Finished { total_successful, total_failed, elapsed } => {
                    println!();
                    println!("=== PROCESSING COMPLETED ===");
                    println!("Total processing time: {:.2}s", elapsed);
                    println!("Demos processed successfully: {}", total_successful);
                    println!("Demos failed: {}", total_failed);

                    let mut s = stats_clone.lock().unwrap();
                    s.insert("successful", total_successful);
                    s.insert("failed", total_failed);
                },
                // Ignore granular events for CLI (or could implement detailed progress bars but simplified for now)
                ProgressEvent::UnzippedFile { .. } |
                ProgressEvent::CollectionWritingProgress { .. } |
                ProgressEvent::TickWritingProgress { .. } => {}
            }
        },
        collection_filter,
        asset_refresh_only,
        input_kind,
        None  // No cancellation token for CLI
    ).await?;

    // Print extended stats from results (since callback doesn't carry full results structs)
    if batch_results.total_aces > 0 || batch_results.total_quads > 0 {
        // Check any stats
        println!("\n--- Kill Collection Processing Complete ---");
        println!(
            "Total Collections Found [+{} ACES][+{} QUAD][+{} TRIPLE][+{} MULTI][+{} DOUBLE]",
            batch_results.total_aces,
            batch_results.total_quads,
            batch_results.total_triples,
            batch_results.total_multis,
            batch_results.total_doubles
        );
    }

    if !tbt_results.is_empty() {
        let mut tbt_counts = HashMap::new();
        for result in &tbt_results {
            *tbt_counts
                .entry(result.collection_type.as_str())
                .or_insert(0) += 1;
        }
        println!("Tick-by-tick processing summary: {} ACEs, {} QUADs, {} TRIPLES, {} MULTIs, {} DOUBLES, {} SINGLES",
            tbt_counts.get("ACE").unwrap_or(&0),
            tbt_counts.get("QUAD").unwrap_or(&0),
            tbt_counts.get("TRIPLE").unwrap_or(&0),
            tbt_counts.get("MULTI").unwrap_or(&0),
            tbt_counts.get("DOUBLE").unwrap_or(&0),
            tbt_counts.get("SINGLE").unwrap_or(&0)
        );
    }

    if !batch_results.errors.is_empty() {
        println!();
        println!("=== ERRORS ===");
        for (path, error) in &batch_results.errors {
            println!("❌ {}: {}", path.display(), error);
        }
    }

    // A GUI child process has no console and never writes stdin. Waiting for Enter there
    // looks like a parse that started and then did nothing.
    if config.batch.autoclose || !std::io::stdin().is_terminal() {
        println!("\nAuto-closing application...");
    } else {
        println!("\nPress Enter to exit...");
        let mut input = String::new();
        let _ = std::io::stdin().read_line(&mut input);
    }

    Ok(())
}

async fn run_fetch_cli(argv: &[String]) -> Result<()> {
    use clap::Parser;
    use demofetch::cli::{Args, Mode};
    use std::sync::atomic::{AtomicBool, Ordering};
    let args = Args::parse_from(std::iter::once("Demoparser fetch").chain(argv[2..].iter().map(String::as_str)));
    let cancel = Arc::new(AtomicBool::new(false));
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() { signal_cancel.store(true, Ordering::Relaxed); }
    });
    let ndjson = args.ndjson;
    let callback: demofetch::Callback = Arc::new(move |event| {
        if ndjson { println!("{}", serde_json::to_string(&event).unwrap()); }
        else if !matches!(event, demofetch::Event::DownloadProgress { .. }) { println!("{event:?}"); }
    });
    // Validate parser config before acquiring anything.
    let config = if args.mode == Mode::Download || args.discover_only { None } else {
        Some(match &args.config { Some(path) => AppConfig::load_from_file(path)?, None => AppConfig::load()? })
    };
    let settings = demofetch::settings::Settings::load(args.credentials.as_deref())?;
    let client = demofetch::FaceitClient::new(settings.credentials)?;
    let matches = args.resolve_matches(&client, &cancel, &callback).await?;
    if args.discover_only {
        if ndjson { println!("{}", serde_json::json!({"type":"fetch_finished", "discover_only":true, "queue":matches})); }
        else { println!("Availability discovery finished; {} matches queued. See faceit-availability.json in the report directory.", matches.len()); }
        return Ok(());
    }
    if args.oldest_available && matches.is_empty() {
        if ndjson { println!("{}", serde_json::json!({"type":"fetch_finished", "queued":0, "reason":"No unsaved downloadable demo confirmed within search budget"})); }
        else { println!("No unsaved downloadable demos confirmed. See faceit-availability.json in the report directory; missing URLs and unchecked rows are not proof of expiry."); }
        return Ok(());
    }
    let request = demoparser::fetch_pipeline::FetchRequest {
        matches, output: args.output.or(settings.output_directory).unwrap_or_else(|| "D:/".into()),
        report_directory: args.report_directory,
        concurrency: args.concurrency.map(usize::from).or(settings.concurrency).unwrap_or(3),
        parse_batch_size: args.parse_batch_size as usize, mode: args.mode,
    };
    let report = demoparser::fetch_pipeline::run_fetch(request, client, config, cancel, callback, move |event| {
        if ndjson { progress_json::emit(&event); }
        else { eprintln!("{event:?}"); }
    }).await?;
    if ndjson { println!("{}", serde_json::json!({"type": "fetch_finished", "report": report})); }
    else { println!("Completed {} matches; {} parser jobs. Report: {}", report.downloads.len(), report.processing.len(), report.report_path.display()); }
    if report.canceled { anyhow::bail!("Canceled; completed demos were preserved"); }
    if report.has_errors() { anyhow::bail!("Some downloads or parser jobs failed; see the per-run report and rerun to retry"); }
    Ok(())
}

/// Reads a collection selection: a JSON object mapping each demo path to the collection numbers
/// wanted from it, as `{"C:/demos/a.dem": [1, 4, 7]}`.
///
/// A file rather than command-line arguments because a selection made in a table is routinely
/// hundreds of entries, which is well past what a command line will carry on Windows. Failing
/// loudly here is deliberate: silently parsing everything because a selection could not be read
/// would be a far longer job than the caller asked for.
fn load_collection_selection(path: &str) -> Result<HashMap<std::path::PathBuf, Vec<u32>>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read the selection file {}: {}", path, e))?;

    let raw: HashMap<String, Vec<u32>> = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("The selection file {} is not valid JSON: {}", path, e))?;

    Ok(raw
        .into_iter()
        .map(|(demo, collections)| (std::path::PathBuf::from(demo), collections))
        .collect())
}
