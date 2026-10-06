//! Decompression utilities for handling compressed demo files
//!
//! Supports .gz and .zst compression formats, with async batch decompression
//! for improved performance in parallel processing scenarios.

use anyhow::{anyhow, Result};
use config::AppConfig;
use flate2::read::GzDecoder;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task;
use zstd::stream::Decoder;

/// Represents decompressed files for a batch
#[derive(Debug)]
pub struct BatchDecompressedFiles {
    pub demo_files: Vec<PathBuf>,
    pub decompressed_files: HashMap<PathBuf, PathBuf>,
    pub failed_files: Vec<(PathBuf, String)>,
}

/// Helper to determine output path based on optional unzip directory
fn determine_output_path(
    source_path: &Path,
    extension: &str,
    unzip_dir: Option<&PathBuf>,
) -> PathBuf {
    let file_name = source_path.file_name().unwrap().to_str().unwrap();
    let output_file_name = file_name.trim_end_matches(extension).trim_end_matches('.');

    if let Some(dir) = unzip_dir {
        // Ensure directory exists
        if !dir.exists() {
            let _ = fs::create_dir_all(dir);
        }
        dir.join(output_file_name)
    } else {
        source_path.with_file_name(output_file_name)
    }
}

/// Decompress a .gz file to a .dem file
pub fn decompress_gz_file(gz_path: &Path, unzip_dir: Option<&PathBuf>) -> Result<PathBuf> {
    let file = fs::File::open(gz_path)?;
    let mut decoder = GzDecoder::new(file);

    let output_path = determine_output_path(gz_path, ".gz", unzip_dir);

    // Create output file
    let mut output_file = fs::File::create(&output_path)?;

    // Copy decompressed data to output file
    std::io::copy(&mut decoder, &mut output_file)?;

    Ok(output_path)
}

/// Decompress a .gz file to memory
pub fn decompress_gz_to_memory(gz_path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(gz_path)?;
    let mut decoder = GzDecoder::new(file);
    let mut buffer = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut buffer)?;
    Ok(buffer)
}

/// Decompress a .zst file to a .dem file
pub fn decompress_zst_file(zst_path: &Path, unzip_dir: Option<&PathBuf>) -> Result<PathBuf> {
    let file = fs::File::open(zst_path)?;
    let mut decoder = Decoder::new(file)?;

    let output_path = determine_output_path(zst_path, ".zst", unzip_dir);

    // Create output file
    let mut output_file = fs::File::create(&output_path)?;

    // Copy decompressed data to output file
    std::io::copy(&mut decoder, &mut output_file)?;

    Ok(output_path)
}

/// Decompress a .zst file to memory
pub fn decompress_zst_to_memory(zst_path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(zst_path)?;
    let mut decoder = Decoder::new(file)?;
    let mut buffer = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut buffer)?;
    Ok(buffer)
}

/// Decompress a file based on its extension
pub fn decompress_file(file_path: &Path, unzip_dir: Option<&PathBuf>) -> Result<PathBuf> {
    match file_path.extension().and_then(|ext| ext.to_str()) {
        Some("gz") => decompress_gz_file(file_path, unzip_dir),
        Some("zst") => decompress_zst_file(file_path, unzip_dir),
        _ => Err(anyhow!(
            "Unsupported compressed file format: {}",
            file_path.display()
        )),
    }
}

/// Decompress files for a batch in the background
pub async fn decompress_batch_async(
    batch: &[PathBuf],
    config: &AppConfig,
    should_skip_fn: impl Fn(&PathBuf, &AppConfig) -> Result<bool> + Send + Sync + 'static,
    pool: Arc<rayon::ThreadPool>,
    progress_callback: Option<Arc<Box<dyn Fn(String) + Send + Sync>>>,
) -> Result<BatchDecompressedFiles> {
    let batch_vec = batch.to_vec();
    let config_clone = config.clone();

    // Offload to blocking thread which installs the Rayon pool (pinned to E-cores)
    let result = task::spawn_blocking(move || {
        pool.install(|| {
            // Process in parallel using Rayon
            // Return type: Option<Result<(PathBuf, Option<PathBuf>), (PathBuf, String)>>
            // None = skipped, Ok = success, Err = failure
            let results: Vec<_> = batch_vec
                .par_iter()
                .map(|path| {
                    // Skip check
                    match should_skip_fn(path, &config_clone) {
                        Ok(true) => {
                            return None;
                        }
                        _ => {}
                    }

                    let result = match path.extension().and_then(|ext| ext.to_str()) {
                        Some("dem") => Some(Ok((path.clone(), None))),
                        Some("gz") | Some("zst") => {
                            let dem_path_source = path.with_extension("dem");
                            if dem_path_source.exists() {
                                Some(Ok((dem_path_source, None)))
                            } else if config_clone.paths.ram_unzip {
                                // RAM mode: Skip decompression in batch phase
                                // The demo will be decompressed on-demand when loading
                                // This is handled by using the compressed path directly
                                Some(Ok((path.clone(), None)))
                            } else {
                                // Disk mode: Decompress to unzip_dir or source folder
                                let unzip_dir = config_clone.paths.unzip_dir.clone();
                                match decompress_file(path, unzip_dir.as_ref()) {
                                    Ok(decompressed_path) => {
                                        // Report successful unzip
                                        if let Some(callback) = &progress_callback {
                                            let filename = path
                                                .file_name()
                                                .and_then(|s| s.to_str())
                                                .unwrap_or("unknown")
                                                .to_string();
                                            callback(filename);
                                        }
                                        Some(Ok((decompressed_path, Some(path.clone()))))
                                    }
                                    Err(e) => {
                                        let error_msg = format!("Decompression failed: {}", e);
                                        eprintln!("Error decompressing {}: {}", path.display(), e);
                                        Some(Err((path.clone(), error_msg)))
                                    }
                                }
                            }
                        }
                        _ => Some(Err((
                            path.clone(),
                            "Unsupported file extension".to_string(),
                        ))),
                    };

                    result
                })
                .collect();

            results
        })
    })
    .await?;

    let mut demo_files = Vec::new();
    let mut decompressed_files = HashMap::new();
    let mut failed_files = Vec::new();

    for res in result {
        match res {
            Some(Ok((path, source_opt))) => {
                demo_files.push(path.clone());
                if let Some(source) = source_opt {
                    decompressed_files.insert(path, source);
                }
            }
            Some(Err((path, error))) => {
                failed_files.push((path, error));
            }
            None => {
                // Skipped - do nothing
            }
        }
    }

    Ok(BatchDecompressedFiles {
        demo_files,
        decompressed_files,
        failed_files,
    })
}
