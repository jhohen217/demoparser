//! Demo file loading and memory mapping utilities
//!
//! This module handles the loading of CS2 demo files and memory mapping
//! for efficient parsing operations. Supports compressed formats (.gz, .zst).

use anyhow::{anyhow, Result};
use flate2::read::GzDecoder;
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Error types for demo loading operations
#[derive(Debug)]
pub enum DemoLoadError {
    FileError(io::Error),
    MapError(io::Error),
}

impl std::fmt::Display for DemoLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DemoLoadError::FileError(e) => write!(f, "Failed to open demo file: {}", e),
            DemoLoadError::MapError(e) => write!(f, "Failed to memory map demo file: {}", e),
        }
    }
}

impl std::error::Error for DemoLoadError {}

impl From<io::Error> for DemoLoadError {
    fn from(error: io::Error) -> Self {
        DemoLoadError::FileError(error)
    }
}

/// Where a loaded demo's bytes actually live.
///
/// Replaces a set of four parallel `Option` fields whose valid combinations were implicit.
/// `Shared` in particular matters: decompressed demos are held in the global cache behind
/// an `Arc`, and the previous code cloned that `Arc` back into a fresh `Vec` on every load
/// (and cloned the buffer a second time when inserting it), costing two full copies of a
/// ~400 MB demo per load. Holding the `Arc` shares the one allocation instead.
enum DemoBacking {
    /// Memory-mapped file. The `File` is retained so the handle outlives the mapping.
    Mapped { _file: File, mmap: Mmap },
    /// Bytes shared with the decompressed-demo cache.
    Shared(Arc<Vec<u8>>),
}

impl DemoBacking {
    fn as_slice(&self) -> &[u8] {
        match self {
            DemoBacking::Mapped { mmap, .. } => mmap,
            DemoBacking::Shared(buffer) => buffer,
        }
    }
}

/// Demo file handle backed by a memory map or by shared in-memory bytes.
///
/// This struct owns whatever keeps the demo's bytes alive and hands out a borrowed slice,
/// so nothing needs to be leaked or copied to satisfy a lifetime.
pub struct DemoFile {
    backing: DemoBacking,
    pub temp_path: Option<PathBuf>, // Track temporary files for cleanup
}

impl Drop for DemoFile {
    fn drop(&mut self) {
        // Clean up temporary files if they exist
        if let Some(temp_path) = &self.temp_path {
            if temp_path.exists() {
                if let Err(e) = std::fs::remove_file(temp_path) {
                    eprintln!(
                        "Warning: Failed to clean up temporary file {:?}: {}",
                        temp_path, e
                    );
                }
            }
        }
    }
}

impl DemoFile {
    /// Get the underlying data as a byte slice, regardless of storage mode
    pub fn data(&self) -> &[u8] {
        self.backing.as_slice()
    }

    /// Create a DemoFile from an in-memory buffer (RAM mode)
    pub fn from_buffer(buffer: Vec<u8>) -> Self {
        DemoFile {
            backing: DemoBacking::Shared(Arc::new(buffer)),
            temp_path: None,
        }
    }

    /// Create a DemoFile from bytes already held by the demo cache, without copying them.
    pub fn from_shared(buffer: Arc<Vec<u8>>) -> Self {
        DemoFile {
            backing: DemoBacking::Shared(buffer),
            temp_path: None,
        }
    }

    /// Load a demo file and create a memory map or buffer
    ///
    /// Opens the specified demo file and creates a memory map for efficient
    /// access during parsing operations. Automatically handles compressed formats (.gz, .zst).
    /// If ram_mode is true (indicated by unzip_dir = None for compressed files), decompresses
    /// directly to memory without creating temporary files.
    ///
    /// # Arguments
    /// * `demo_path` - Path to the demo file to load
    /// * `unzip_dir` - Optional directory for temporary decompressed files (None = RAM mode)
    ///
    /// # Returns
    /// * `Ok(DemoFile)` on success
    /// * `Err(DemoLoadError)` if the file cannot be opened or mapped
    ///
    /// # Example
    /// ```no_run
    /// use demoparser::tick_by_tick::demo_loader::DemoFile;
    ///
    /// let demo = DemoFile::load("path/to/demo.dem", None).unwrap();
    /// // Use demo.data() for parsing
    /// ```
    pub fn load(demo_path: &str, unzip_dir: Option<&Path>) -> Result<Self, DemoLoadError> {
        let path = Path::new(demo_path);

        // Check if this is a compressed file that should be extracted
        if is_compressed_file(path) {
            // RAM mode: decompress directly to memory (no temp files)
            if unzip_dir.is_none() {
                // Try to get from cache first
                let path_str = path.to_string_lossy().to_string();
                if let Some(cached) = interface::demo_cache::get_cached_demo(&path_str) {
                    // Share the cache's allocation rather than copying it.
                    return Ok(DemoFile::from_shared(cached));
                }

                let buffer = extract_to_memory(path).map_err(|e| {
                    DemoLoadError::FileError(io::Error::new(io::ErrorKind::Other, e))
                })?;

                // cache_demo takes ownership and hands back the shared handle, so the
                // buffer moves in and is never duplicated.
                let shared = interface::demo_cache::cache_demo(path_str, buffer);

                return Ok(DemoFile::from_shared(shared));
            } else {
                // Disk mode: decompress to temp file
                println!("Detected compressed file, extracting...");
                let extracted_path = extract_compressed_demo(path, unzip_dir).map_err(|e| {
                    DemoLoadError::FileError(io::Error::new(io::ErrorKind::Other, e))
                })?;

                let file = File::open(&extracted_path).map_err(DemoLoadError::FileError)?;

                let mmap = unsafe {
                    MmapOptions::new()
                        .map(&file)
                        .map_err(DemoLoadError::MapError)?
                };

                return Ok(DemoFile {
                    backing: DemoBacking::Mapped { _file: file, mmap },
                    temp_path: Some(extracted_path),
                });
            }
        }

        // Uncompressed file: use memory mapping
        let file = File::open(path).map_err(DemoLoadError::FileError)?;

        let mmap = unsafe {
            MmapOptions::new()
                .map(&file)
                .map_err(DemoLoadError::MapError)?
        };

        Ok(DemoFile {
            backing: DemoBacking::Mapped { _file: file, mmap },
            temp_path: None,
        })
    }

    /// Check if the demo file appears to be a valid CS2 demo
    ///
    /// Performs validation to ensure the file has the correct CS2 demo format.
    /// Checks for proper magic bytes and minimum file size.
    ///
    /// # Returns
    /// * `true` if the file appears to be a valid CS2 demo
    /// * `false` if the file appears to be invalid or corrupted
    pub fn validate(&self) -> bool {
        let data = self.data();

        // Check minimum file size (demos should be at least a few KB)
        if data.len() < 1024 {
            return false;
        }

        // Check for CS2 demo header magic bytes
        if data.len() >= 8 {
            let header = &data[0..8];

            // Check for known CS2/Source 2 demo signatures
            if header.starts_with(b"HL2DEMO\0") {
                return true; // Half-Life 2/Source demo format
            }

            if header.starts_with(b"PBDEMS2\0") {
                return true; // Source 2 demo format
            }

            // Check for other potential CS2 demo signatures
            if header.starts_with(b"PBDEM\0\0\0") {
                return true; // Alternative Source demo format
            }

            // Log the actual header for debugging
            println!(
                "Warning: Unknown demo header: {:?}",
                std::str::from_utf8(&header[0..4]).unwrap_or("invalid UTF-8")
            );
            println!("Header bytes: {:02X?}", &header[0..8]);
        }

        false
    }
}

/// Check if a file is compressed based on its extension
fn is_compressed_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        matches!(ext.to_str(), Some("gz") | Some("zst"))
    } else {
        false
    }
}

/// Extract a compressed demo file to a temporary location
fn extract_compressed_demo(compressed_path: &Path, unzip_dir: Option<&Path>) -> Result<PathBuf> {
    let file_name = compressed_path
        .file_stem()
        .ok_or_else(|| anyhow!("Invalid file name"))?
        .to_str()
        .ok_or_else(|| anyhow!("Invalid UTF-8 in file name"))?;

    // Determine output directory: user provided > system temp
    let output_dir = if let Some(dir) = unzip_dir {
        if !dir.exists() {
            let _ = std::fs::create_dir_all(dir);
        }
        dir.to_path_buf()
    } else {
        std::env::temp_dir()
    };

    // Create a temporary filename with .dem extension
    // If using scratch dir, we might want to avoid random IDs to reuse if needed,
    // but for safety/concurrency we keep unique names or use original name?
    // The original logic used process_id. We'll keep that to avoid collisions.
    let temp_path = output_dir.join(format!("{}_{}.dem", file_name, std::process::id()));

    let extension = compressed_path
        .extension()
        .and_then(|ext| ext.to_str())
        .ok_or_else(|| anyhow!("Invalid file extension"))?;

    match extension {
        "gz" => extract_gzip(compressed_path, &temp_path)?,
        "zst" => extract_zstd(compressed_path, &temp_path)?,
        _ => return Err(anyhow!("Unsupported compression format: {}", extension)),
    }

    println!("Extracted compressed demo to: {:?}", temp_path);
    Ok(temp_path)
}

/// Extract a compressed file directly to memory (RAM mode)
fn extract_to_memory(compressed_path: &Path) -> Result<Vec<u8>> {
    let extension = compressed_path
        .extension()
        .and_then(|ext| ext.to_str())
        .ok_or_else(|| anyhow!("Invalid file extension"))?;

    let compressed_size = std::fs::metadata(compressed_path)?.len();
    println!(
        "DEBUG: Decompressing {} (compressed size: {} bytes)",
        compressed_path.display(),
        compressed_size
    );

    match extension {
        "gz" => {
            let input_file = File::open(compressed_path)?;
            let mut decoder = GzDecoder::new(input_file);
            let mut buffer = Vec::new();
            let bytes_read = io::Read::read_to_end(&mut decoder, &mut buffer)?;
            println!("DEBUG: Decompressed {} bytes from .gz file", bytes_read);
            if buffer.len() < 1024 {
                return Err(anyhow!(
                    "Decompressed buffer suspiciously small: {} bytes",
                    buffer.len()
                ));
            }
            Ok(buffer)
        }
        "zst" => {
            let input_file = File::open(compressed_path)?;
            let mut decoder = zstd::Decoder::new(input_file)?;
            let mut buffer = Vec::new();
            let bytes_read = io::Read::read_to_end(&mut decoder, &mut buffer)?;
            println!("DEBUG: Decompressed {} bytes from .zst file", bytes_read);
            if buffer.len() < 1024 {
                return Err(anyhow!(
                    "Decompressed buffer suspiciously small: {} bytes",
                    buffer.len()
                ));
            }
            Ok(buffer)
        }
        _ => Err(anyhow!("Unsupported compression format: {}", extension)),
    }
}

/// Extract a gzip compressed file
fn extract_gzip(input_path: &Path, output_path: &Path) -> Result<()> {
    let input_file = File::open(input_path)?;
    let mut decoder = GzDecoder::new(input_file);
    let mut output_file = File::create(output_path)?;

    io::copy(&mut decoder, &mut output_file)?;
    Ok(())
}

/// Extract a zstd compressed file
fn extract_zstd(input_path: &Path, output_path: &Path) -> Result<()> {
    let input_file = File::open(input_path)?;
    let mut decoder = zstd::Decoder::new(input_file)?;
    let mut output_file = File::create(output_path)?;

    io::copy(&mut decoder, &mut output_file)?;
    Ok(())
}

/// Load and validate a demo file
///
/// Convenience function that loads a demo file and performs basic validation.
/// Handles compressed files automatically.
///
/// # Arguments
/// * `demo_path` - Path to the demo file to load
///
/// # Returns
/// * `Ok(DemoFile)` if the file is loaded and appears valid
/// * `Err(DemoLoadError)` if loading fails or validation fails
pub fn load_and_validate_demo(
    demo_path: &str,
    unzip_dir: Option<&Path>,
) -> Result<DemoFile, DemoLoadError> {
    let demo = DemoFile::load(demo_path, unzip_dir)?;

    if !demo.validate() {
        return Err(DemoLoadError::FileError(io::Error::new(
            io::ErrorKind::InvalidData,
            "Demo file appears to be invalid or corrupted",
        )));
    }

    Ok(demo)
}

/// Find the actual demo file path from a demo path that might be compressed
///
/// This function resolves the demo path from kill collection data, handling
/// cases where the demo might be in compressed format.
///
/// # Arguments
/// * `demo_path` - The demo path from kill collection data
///
/// # Returns
/// * The actual path to use for loading (might be the same if uncompressed exists)
/// * `Err` if no valid demo file can be found
pub fn resolve_demo_path(demo_path: &str, unzip_dir: Option<&Path>) -> Result<String> {
    let path = Path::new(demo_path);

    // 1. Check unzip_dir for already decomprssed file
    if let Some(dir) = unzip_dir {
        if let Some(_file_name) = path.file_stem() {
            // If demo_path is "foo.dem.gz", stem is "foo.dem"
            // If demo_path is "foo.dem", stem is "foo"
            // We want "foo.dem"
            let name_str = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

            // Check if input path is compressed
            if name_str.ends_with(".gz") || name_str.ends_with(".zst") {
                // Try to see if the stem exists in unzip_dir
                // e.g. foo.dem.gz -> foo.dem
                // This doesn't handle .dem perfectly if stem strips .dem
                // Path::new("foo.dem.gz").file_stem() -> "foo.dem"
                // Path::new("foo.dem").file_stem() -> "foo"

                let decompressed_name = if let Some(stem) = path.file_stem() {
                    PathBuf::from(stem)
                } else {
                    PathBuf::from(path)
                };

                let candidate = dir.join(&decompressed_name);
                if candidate.exists() {
                    return Ok(candidate.to_string_lossy().to_string());
                }
            }
        }
    }

    // If the exact path exists, use it
    if path.exists() {
        return Ok(demo_path.to_string());
    }

    // Try with .gz extension
    let gz_path = format!("{}.gz", demo_path);
    if Path::new(&gz_path).exists() {
        return Ok(gz_path);
    }

    // Try with .zst extension
    let zst_path = format!("{}.zst", demo_path);
    if Path::new(&zst_path).exists() {
        return Ok(zst_path);
    }

    // If the original path has .dem extension, try without it and add compression extensions
    if let Some(_stem) = path.file_stem() {
        if demo_path.ends_with(".dem") {
            let base_path = demo_path.strip_suffix(".dem").unwrap();

            let gz_path = format!("{}.gz", base_path);
            if Path::new(&gz_path).exists() {
                return Ok(gz_path);
            }

            let zst_path = format!("{}.zst", base_path);
            if Path::new(&zst_path).exists() {
                return Ok(zst_path);
            }
        }
    }

    Err(anyhow!(
        "Demo file not found: {} (also tried .gz and .zst variants)",
        demo_path
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_nonexistent_file() {
        let result = DemoFile::load("nonexistent_file.dem", None);
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), DemoLoadError::FileError(_)));
    }

    /// 2 KB starting with the Source 2 magic, which is what a real demo begins with.
    fn source2_demo_bytes() -> Vec<u8> {
        let mut data = Vec::with_capacity(2048);
        data.extend_from_slice(b"PBDEMS2\0");
        data.resize(2048, 0u8);
        data
    }

    #[test]
    fn test_load_valid_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&source2_demo_bytes()).unwrap();

        let result = DemoFile::load(temp_file.path().to_str().unwrap(), None);
        assert!(result.is_ok());

        let demo = result.unwrap();
        assert_eq!(demo.data().len(), 2048);
        assert!(demo.validate());
    }

    /// Large enough to pass the size check, but not a demo. `validate` gates on the magic,
    /// not just the length - this is what the previous version of this test asserted the
    /// opposite of, and it never ran because the crate's tests did not compile.
    #[test]
    fn test_validate_rejects_unknown_header() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(&vec![0u8; 2048]).unwrap();

        let demo = DemoFile::load(temp_file.path().to_str().unwrap(), None).unwrap();
        assert!(!demo.validate());
    }

    #[test]
    fn test_validate_small_file() {
        // Create a very small file
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = vec![0u8; 100]; // Only 100 bytes
        temp_file.write_all(&test_data).unwrap();

        let demo = DemoFile::load(temp_file.path().to_str().unwrap(), None).unwrap();
        assert!(!demo.validate()); // Should fail validation due to small size
    }
}
