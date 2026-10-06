//! Crash-safe file writing.
//!
//! Outputs used to be written straight to their final paths. A crash, kill or full disk
//! mid-write left a truncated file that the skip check on the next run treated as a
//! complete output, so the collection was never regenerated and the database reported it
//! present. Writing to a temporary file in the same directory and renaming into place
//! means a partial write is never visible under the real name.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Temporary path alongside `final_path`, in the same directory so the rename stays on one
/// volume (a cross-volume rename is a copy and loses atomicity). The process id keeps
/// concurrent runs over the same output directory from colliding.
fn temp_path_for(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let temp_name = format!(".{}.{}.tmp", file_name, std::process::id());
    match final_path.parent() {
        Some(dir) => dir.join(temp_name),
        None => PathBuf::from(temp_name),
    }
}

/// Run `write` against a temporary path, then atomically move the result to `final_path`.
///
/// The temporary file is removed if `write` fails or produces nothing, so a failed run
/// leaves no debris. `final_path`'s parent directory is created if needed.
pub fn write_atomically<F>(final_path: &Path, write: F) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory {}", parent.display()))?;
    }

    let temp_path = temp_path_for(final_path);
    // A temp file left by a previous crashed run must not be mistaken for this run's output.
    let _ = std::fs::remove_file(&temp_path);

    let result = write(&temp_path).and_then(|()| {
        let len = std::fs::metadata(&temp_path)
            .with_context(|| {
                format!(
                    "Writer reported success but {} is missing",
                    temp_path.display()
                )
            })?
            .len();
        if len == 0 {
            return Err(anyhow!(
                "Writer produced an empty file for {}",
                final_path.display()
            ));
        }
        Ok(())
    });

    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    // std::fs::rename replaces an existing destination on both Windows and Unix.
    std::fs::rename(&temp_path, final_path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        anyhow!(
            "Failed to move {} into place at {}: {}",
            temp_path.display(),
            final_path.display(),
            e
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn successful_write_lands_at_the_final_path() {
        let dir = temp_dir();
        let target = dir.path().join("out.bin");

        write_atomically(&target, |p| {
            let mut f = std::fs::File::create(p)?;
            f.write_all(b"payload")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
    }

    #[test]
    fn failed_write_leaves_no_file_and_no_debris() {
        let dir = temp_dir();
        let target = dir.path().join("out.bin");

        let err = write_atomically(&target, |p| {
            let mut f = std::fs::File::create(p)?;
            f.write_all(b"partial")?;
            Err(anyhow!("writer exploded"))
        });

        assert!(err.is_err());
        assert!(
            !target.exists(),
            "partial output must not appear under the real name"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }

    #[test]
    fn empty_output_is_rejected_rather_than_published() {
        let dir = temp_dir();
        let target = dir.path().join("out.bin");

        let err = write_atomically(&target, |p| {
            std::fs::File::create(p)?;
            Ok(())
        });

        assert!(err.is_err());
        assert!(!target.exists());
    }

    #[test]
    fn an_existing_output_is_replaced() {
        let dir = temp_dir();
        let target = dir.path().join("out.bin");
        std::fs::write(&target, b"stale").unwrap();

        write_atomically(&target, |p| {
            std::fs::write(p, b"fresh")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
    }

    /// A truncated file from a previous crash must not be adopted as this run's output.
    #[test]
    fn stale_temp_file_from_a_previous_run_is_discarded() {
        let dir = temp_dir();
        let target = dir.path().join("out.bin");
        std::fs::write(temp_path_for(&target), b"leftover garbage").unwrap();

        write_atomically(&target, |p| {
            std::fs::write(p, b"fresh")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
    }
}
