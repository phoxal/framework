//! Artifact publication for the scenario surface.
//!
//! `Program::write_to` persists the canonical program bytes to disk.
//! The publication helper centralises the atomic-write pattern so the
//! scenario surface does not rely on a shared temporary filename the
//! way the previous handwritten `write_atomic` did.

use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug)]
pub enum PublicationError {
    Io(std::io::Error),
    Persist {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for PublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(source) => write!(f, "{source}"),
            Self::Persist { path, source } => {
                write!(f, "cannot persist {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PublicationError {}

/// Persist the canonical program bytes atomically. The destination is
/// replaced only after the temp file has been flushed and renamed.
pub fn write_program_bytes(path: &Path, bytes: &[u8]) -> Result<(), PublicationError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(PublicationError::Io)?;
    temporary.write_all(bytes).map_err(PublicationError::Io)?;
    temporary.as_file().sync_all().map_err(PublicationError::Io)?;
    temporary
        .persist(path)
        .map_err(|error| PublicationError::Persist {
            path: path.to_owned(),
            source: error.error,
        })?;
    if let Err(source) = fs::File::open(parent).and_then(|file| file.sync_all()) {
        // Parent-dir sync is best-effort: a missing parent is not a hard
        // error because the temp file lived in the same directory.
        if source.kind() != std::io::ErrorKind::NotFound {
            return Err(PublicationError::Io(source));
        }
    }
    Ok(())
}
