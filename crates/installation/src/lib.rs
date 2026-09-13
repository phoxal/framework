//! Artifact-only deployment release installation.
//!
//! A release contains exactly the supervisor executable and its compiled
//! bundle:
//!
//! ```text
//! phoxal-supervisor
//! bundle/
//!   manifest.json
//!   assets/
//!   bin/
//! ```
//!
//! The source compiler owns construction and graph validation.
//! This crate owns deterministic archiving, integrity verification, bounded
//! extraction, immutable content-addressed storage, and exact activation.
//! It never invokes Cargo or reads authored project files.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tar::{Archive, Builder, EntryType, Header};

mod operations;
mod service;
pub use operations::{
    DEFAULT_LOG_LINES, DEFAULT_MAX_LOG_BYTES, DeploymentOperations, DeploymentStatus, LogQuery,
    OperationsError, ProcessState, ServiceControl, ServiceControlError, ServiceLogs, ServiceStatus,
    SystemdService,
};
pub use service::{
    DeploymentIdentity, IdentityUpdate, SERVICE_UNIT_FILE, ServiceConfigError,
    configure_systemd_service, read_systemd_identity, render_systemd_service,
};

/// The supervisor executable stored beside every compiled bundle.
pub const SUPERVISOR_FILE: &str = "phoxal-supervisor";
/// The compiled runtime bundle directory.
pub const BUNDLE_DIR: &str = "bundle";
/// The compiled bundle manifest.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The immutable installed release directory.
pub const RELEASES_DIR: &str = "releases";
/// The symlink selecting the active immutable release.
pub const ACTIVE_LINK: &str = "active";

/// Default defensive limits for an extracted deployment release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractionLimits {
    /// Maximum regular-file count.
    pub files: u64,
    /// Maximum sum of declared regular-file bytes.
    pub bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            files: 65_536,
            bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

/// The SHA-256 identity of one immutable release archive.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseId(String);

impl ReleaseId {
    /// Parse a lowercase hexadecimal SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns [`InstallationError::InvalidDigest`] when `value` is not exactly
    /// 64 lowercase hexadecimal characters.
    pub fn parse(value: impl Into<String>) -> Result<Self, InstallationError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InstallationError::InvalidDigest { value });
        }
        Ok(Self(value))
    }

    /// The canonical lowercase digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ReleaseId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A validated deployment release on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseLayout {
    root: PathBuf,
    supervisor: PathBuf,
    bundle: PathBuf,
}

impl ReleaseLayout {
    /// Validate the fixed release layout without consulting source files.
    ///
    /// # Errors
    ///
    /// Returns a typed layout, filesystem, executable, or manifest error.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, InstallationError> {
        let root = root.as_ref();
        let metadata = fs::symlink_metadata(root).map_err(|source| InstallationError::Inspect {
            path: root.to_owned(),
            source,
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(InstallationError::NotDirectory {
                path: root.to_owned(),
            });
        }

        let mut present = BTreeSet::new();
        for entry in fs::read_dir(root).map_err(|source| InstallationError::ReadDirectory {
            path: root.to_owned(),
            source,
        })? {
            let entry = entry.map_err(|source| InstallationError::ReadDirectory {
                path: root.to_owned(),
                source,
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| InstallationError::NonUtf8Entry { path: entry.path() })?;
            if !matches!(name.as_str(), SUPERVISOR_FILE | BUNDLE_DIR) {
                return Err(InstallationError::UnexpectedReleaseEntry { path: entry.path() });
            }
            let entry_metadata = fs::symlink_metadata(entry.path()).map_err(|source| {
                InstallationError::Inspect {
                    path: entry.path(),
                    source,
                }
            })?;
            if entry_metadata.file_type().is_symlink() {
                return Err(InstallationError::UnsupportedFilesystemEntry { path: entry.path() });
            }
            present.insert(name);
        }
        for required in [SUPERVISOR_FILE, BUNDLE_DIR] {
            if !present.contains(required) {
                return Err(InstallationError::MissingReleaseEntry {
                    root: root.to_owned(),
                    name: required,
                });
            }
        }

        let supervisor = root.join(SUPERVISOR_FILE);
        ensure_executable(&supervisor)?;
        let bundle = root.join(BUNDLE_DIR);
        ensure_bundle(&bundle)?;
        Ok(Self {
            root: root.to_owned(),
            supervisor,
            bundle,
        })
    }

    /// Release root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Supervisor executable path.
    #[must_use]
    pub fn supervisor(&self) -> &Path {
        &self.supervisor
    }

    /// Compiled bundle root.
    #[must_use]
    pub fn bundle(&self) -> &Path {
        &self.bundle
    }
}

/// A content-addressed release store.
#[derive(Clone, Debug)]
pub struct Installation {
    root: PathBuf,
    limits: ExtractionLimits,
}

impl Installation {
    /// Open or create a release store.
    ///
    /// # Errors
    ///
    /// Returns [`InstallationError::CreateDirectory`] when its immutable
    /// release directory cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, InstallationError> {
        Self::with_limits(root, ExtractionLimits::default())
    }

    /// Open or create a release store with caller-selected finite limits.
    ///
    /// # Errors
    ///
    /// Returns [`InstallationError::InvalidLimits`] for a zero bound or a
    /// filesystem error when the store cannot be created.
    pub fn with_limits(
        root: impl AsRef<Path>,
        limits: ExtractionLimits,
    ) -> Result<Self, InstallationError> {
        if limits.files == 0 || limits.bytes == 0 {
            return Err(InstallationError::InvalidLimits);
        }
        let root = root.as_ref().to_owned();
        let releases = root.join(RELEASES_DIR);
        fs::create_dir_all(&releases).map_err(|source| InstallationError::CreateDirectory {
            path: releases,
            source,
        })?;
        Ok(Self { root, limits })
    }

    /// Installation root containing immutable releases, the active link, and
    /// generated host service configuration.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Verify and install an archive under its digest without activating it.
    ///
    /// Identical repeated installation is idempotent.
    /// The source archive and checksum sidecar are never modified.
    ///
    /// # Errors
    ///
    /// Returns a typed checksum, archive, layout, bound, or filesystem error.
    pub fn install(
        &self,
        archive: impl AsRef<Path>,
        checksum: impl AsRef<Path>,
    ) -> Result<InstalledRelease, InstallationError> {
        let archive = archive.as_ref();
        let expected = read_checksum(checksum.as_ref())?;
        let actual = digest_file(archive)?;
        if expected != actual {
            return Err(InstallationError::ChecksumMismatch { expected, actual });
        }
        let id = ReleaseId::parse(actual)?;
        let destination = self.release_path(&id);
        if destination.exists() {
            let layout = ReleaseLayout::open(&destination)?;
            return Ok(InstalledRelease { id, layout });
        }

        let releases = self.root.join(RELEASES_DIR);
        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&releases)
            .map_err(|source| InstallationError::CreateStaging {
                path: releases.clone(),
                source,
            })?;
        extract_archive(archive, staging.path(), self.limits)?;
        let _ = ReleaseLayout::open(staging.path())?;
        let staged = staging.keep();
        match fs::rename(&staged, &destination) {
            Ok(()) => {}
            Err(source) if destination.exists() => {
                fs::remove_dir_all(&staged).map_err(|cleanup| {
                    InstallationError::CleanupStaging {
                        path: staged.clone(),
                        source: cleanup,
                    }
                })?;
                let _ = source;
            }
            Err(source) => {
                return Err(InstallationError::PublishRelease {
                    from: staged,
                    to: destination,
                    source,
                });
            }
        }
        let layout = ReleaseLayout::open(&destination)?;
        Ok(InstalledRelease { id, layout })
    }

    /// Atomically select an already installed immutable release.
    ///
    /// Returns the previously active release, if any, so the caller can restore
    /// it when process readiness fails.
    ///
    /// # Errors
    ///
    /// Returns a typed release or symlink publication error.
    pub fn activate(&self, id: &ReleaseId) -> Result<Activation, InstallationError> {
        let target = self.release_path(id);
        let _ = ReleaseLayout::open(&target)?;
        let previous = self.active_id()?;
        atomic_symlink(
            &self.root.join(ACTIVE_LINK),
            Path::new(RELEASES_DIR).join(id.as_str()),
        )?;
        Ok(Activation {
            active: id.clone(),
            previous,
        })
    }

    /// Restore one exact installed release after a failed activation.
    ///
    /// This is intentionally an exact identity rather than an implicit mutable
    /// history stack.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::activate`].
    pub fn rollback(&self, id: &ReleaseId) -> Result<Activation, InstallationError> {
        self.activate(id)
    }

    /// Remove the active selection after a failed first activation.
    ///
    /// This only removes the selector symlink and never deletes an immutable
    /// release.
    ///
    /// # Errors
    ///
    /// Returns a typed active-target or filesystem error.
    pub fn deactivate(&self) -> Result<Option<ReleaseId>, InstallationError> {
        let active = self.root.join(ACTIVE_LINK);
        let previous = self.active_id()?;
        if previous.is_some() {
            fs::remove_file(&active).map_err(|source| InstallationError::Deactivate {
                path: active,
                source,
            })?;
        }
        Ok(previous)
    }

    /// Resolve the currently active immutable release identity.
    ///
    /// # Errors
    ///
    /// Returns an invalid-target error for a link outside this store or a
    /// malformed release identifier.
    pub fn active_id(&self) -> Result<Option<ReleaseId>, InstallationError> {
        let active = self.root.join(ACTIVE_LINK);
        let target = match fs::read_link(&active) {
            Ok(target) => target,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(InstallationError::ReadActive {
                    path: active,
                    source,
                });
            }
        };
        let mut components = target.components();
        let release_dir = components.next();
        let release_id = components.next();
        if release_dir != Some(Component::Normal(RELEASES_DIR.as_ref()))
            || components.next().is_some()
        {
            return Err(InstallationError::InvalidActiveTarget { target });
        }
        let Some(Component::Normal(release_id)) = release_id else {
            return Err(InstallationError::InvalidActiveTarget { target });
        };
        let value = release_id
            .to_str()
            .ok_or_else(|| InstallationError::InvalidActiveTarget {
                target: target.clone(),
            })?;
        ReleaseId::parse(value.to_owned()).map(Some)
    }

    /// Resolve one immutable release directory.
    #[must_use]
    pub fn release_path(&self, id: &ReleaseId) -> PathBuf {
        self.root.join(RELEASES_DIR).join(id.as_str())
    }
}

/// One installed release and its validated paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRelease {
    /// Content identity.
    pub id: ReleaseId,
    /// Validated release layout.
    pub layout: ReleaseLayout,
}

/// Result of selecting an installed release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    /// Newly active release.
    pub active: ReleaseId,
    /// Previously active release, suitable for explicit rollback.
    pub previous: Option<ReleaseId>,
}

/// Create a deterministic deployment archive and its checksum sidecar.
///
/// # Errors
///
/// Returns a typed layout, archive, or filesystem error.
pub fn create_archive(
    release: impl AsRef<Path>,
    archive: impl AsRef<Path>,
    checksum: impl AsRef<Path>,
) -> Result<ReleaseId, InstallationError> {
    let release = release.as_ref();
    let _ = ReleaseLayout::open(release)?;
    let archive = archive.as_ref();
    let file = fs::File::create(archive).map_err(|source| InstallationError::CreateArchive {
        path: archive.to_owned(),
        source,
    })?;
    let encoder = GzEncoder::new(file, Compression::best());
    let mut builder = Builder::new(encoder);
    builder.mode(tar::HeaderMode::Deterministic);
    let mut entries = collect_entries(release)?;
    entries.sort();
    for relative in entries {
        append_entry(&mut builder, release, &relative)?;
    }
    let encoder = builder
        .into_inner()
        .map_err(|source| InstallationError::WriteArchive {
            path: archive.to_owned(),
            source,
        })?;
    let mut file = encoder
        .finish()
        .map_err(|source| InstallationError::WriteArchive {
            path: archive.to_owned(),
            source,
        })?;
    file.flush()
        .map_err(|source| InstallationError::WriteArchive {
            path: archive.to_owned(),
            source,
        })?;
    file.sync_all()
        .map_err(|source| InstallationError::WriteArchive {
            path: archive.to_owned(),
            source,
        })?;
    let id = ReleaseId::parse(digest_file(archive)?)?;
    fs::write(checksum.as_ref(), format!("{}\n", id.as_str())).map_err(|source| {
        InstallationError::WriteChecksum {
            path: checksum.as_ref().to_owned(),
            source,
        }
    })?;
    Ok(id)
}

fn ensure_executable(path: &Path) -> Result<(), InstallationError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| InstallationError::Inspect {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(InstallationError::UnsupportedFilesystemEntry {
            path: path.to_owned(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(InstallationError::NotExecutable {
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

fn ensure_bundle(path: &Path) -> Result<(), InstallationError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| InstallationError::Inspect {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(InstallationError::NotDirectory {
            path: path.to_owned(),
        });
    }
    let manifest = path.join(MANIFEST_FILE);
    let bytes = fs::read(&manifest).map_err(|source| InstallationError::ReadManifest {
        path: manifest.clone(),
        source,
    })?;
    let document = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|source| {
        InstallationError::InvalidManifest {
            path: manifest.clone(),
            source,
        }
    })?;
    if !document.is_object() {
        return Err(InstallationError::ManifestNotObject { path: manifest });
    }
    Ok(())
}

fn collect_entries(root: &Path) -> Result<Vec<PathBuf>, InstallationError> {
    let mut pending = vec![PathBuf::new()];
    let mut entries = Vec::new();
    while let Some(relative) = pending.pop() {
        let directory = root.join(&relative);
        for entry in
            fs::read_dir(&directory).map_err(|source| InstallationError::ReadDirectory {
                path: directory.clone(),
                source,
            })?
        {
            let entry = entry.map_err(|source| InstallationError::ReadDirectory {
                path: directory.clone(),
                source,
            })?;
            let child = relative.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path()).map_err(|source| {
                InstallationError::Inspect {
                    path: entry.path(),
                    source,
                }
            })?;
            if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
                return Err(InstallationError::UnsupportedFilesystemEntry { path: entry.path() });
            }
            entries.push(child.clone());
            if metadata.is_dir() {
                pending.push(child);
            }
        }
    }
    Ok(entries)
}

fn append_entry(
    builder: &mut Builder<GzEncoder<fs::File>>,
    root: &Path,
    relative: &Path,
) -> Result<(), InstallationError> {
    let source = root.join(relative);
    let metadata = fs::metadata(&source).map_err(|source_error| InstallationError::Inspect {
        path: source.clone(),
        source: source_error,
    })?;
    let mut header = Header::new_gnu();
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header
        .set_username("")
        .map_err(|source_error| InstallationError::WriteArchive {
            path: source.clone(),
            source: source_error,
        })?;
    header
        .set_groupname("")
        .map_err(|source_error| InstallationError::WriteArchive {
            path: source.clone(),
            source: source_error,
        })?;
    if metadata.is_dir() {
        header.set_entry_type(EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, relative, io::empty())
            .map_err(|source_error| InstallationError::WriteArchive {
                path: source,
                source: source_error,
            })?;
        return Ok(());
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = relative == Path::new(SUPERVISOR_FILE)
        || relative.starts_with(Path::new(BUNDLE_DIR).join("bin"));
    header.set_entry_type(EntryType::Regular);
    header.set_mode(if executable { 0o755 } else { 0o644 });
    header.set_size(metadata.len());
    header.set_cksum();
    let mut file = fs::File::open(&source).map_err(|source_error| InstallationError::ReadFile {
        path: source.clone(),
        source: source_error,
    })?;
    builder
        .append_data(&mut header, relative, &mut file)
        .map_err(|source_error| InstallationError::WriteArchive {
            path: source,
            source: source_error,
        })?;
    Ok(())
}

fn extract_archive(
    archive: &Path,
    destination: &Path,
    limits: ExtractionLimits,
) -> Result<(), InstallationError> {
    let file = fs::File::open(archive).map_err(|source| InstallationError::ReadFile {
        path: archive.to_owned(),
        source,
    })?;
    let mut archive_reader = Archive::new(GzDecoder::new(file));
    let entries = archive_reader
        .entries()
        .map_err(|source| InstallationError::ReadArchive {
            path: archive.to_owned(),
            source,
        })?;
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut seen = BTreeSet::new();
    for entry in entries {
        let mut entry = entry.map_err(|source| InstallationError::ReadArchive {
            path: archive.to_owned(),
            source,
        })?;
        let relative = entry
            .path()
            .map_err(|source| InstallationError::ReadArchive {
                path: archive.to_owned(),
                source,
            })?
            .into_owned();
        if !safe_relative(&relative) {
            return Err(InstallationError::UnsafeArchivePath { path: relative });
        }
        if !seen.insert(relative.clone()) {
            return Err(InstallationError::DuplicateArchivePath { path: relative });
        }
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_dir() || entry_type.is_file()) {
            return Err(InstallationError::UnsupportedArchiveEntry { path: relative });
        }
        let output = destination.join(&relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&output).map_err(|source| InstallationError::CreateDirectory {
                path: output,
                source,
            })?;
            continue;
        }
        files = files.saturating_add(1);
        bytes = bytes.saturating_add(entry.size());
        if files > limits.files || bytes > limits.bytes {
            return Err(InstallationError::ExtractionLimit {
                files,
                bytes,
                limits,
            });
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|source| InstallationError::CreateDirectory {
                path: parent.to_owned(),
                source,
            })?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(entry.header().mode().unwrap_or(0o644) & 0o777);
        }
        let mut output_file =
            options
                .open(&output)
                .map_err(|source| InstallationError::WriteFile {
                    path: output.clone(),
                    source,
                })?;
        let copied = io::copy(&mut entry, &mut output_file).map_err(|source| {
            InstallationError::WriteFile {
                path: output.clone(),
                source,
            }
        })?;
        if copied != entry.size() {
            return Err(InstallationError::ArchiveSizeMismatch {
                path: relative,
                expected: entry.size(),
                actual: copied,
            });
        }
        output_file
            .sync_all()
            .map_err(|source| InstallationError::WriteFile {
                path: output,
                source,
            })?;
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn digest_file(path: &Path) -> Result<String, InstallationError> {
    let mut file = fs::File::open(path).map_err(|source| InstallationError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut digest).map_err(|source| InstallationError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    Ok(format!("{:x}", digest.finalize()))
}

fn read_checksum(path: &Path) -> Result<String, InstallationError> {
    let value = fs::read_to_string(path).map_err(|source| InstallationError::ReadChecksum {
        path: path.to_owned(),
        source,
    })?;
    let digest = value.split_whitespace().next().unwrap_or_default();
    ReleaseId::parse(digest.to_owned()).map(|id| id.0)
}

fn atomic_symlink(link: &Path, target: PathBuf) -> Result<(), InstallationError> {
    let parent = link
        .parent()
        .ok_or_else(|| InstallationError::InvalidActivePath {
            path: link.to_owned(),
        })?;
    let temporary = tempfile::Builder::new()
        .prefix(".active-")
        .tempfile_in(parent)
        .map_err(|source| InstallationError::CreateStaging {
            path: parent.to_owned(),
            source,
        })?;
    let temporary_path = temporary.into_temp_path();
    fs::remove_file(&temporary_path).map_err(|source| InstallationError::RemoveTemporary {
        path: temporary_path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &temporary_path).map_err(|source| {
        InstallationError::CreateSymlink {
            path: temporary_path.to_path_buf(),
            target: target.clone(),
            source,
        }
    })?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&target, &temporary_path).map_err(|source| {
        InstallationError::CreateSymlink {
            path: temporary_path.to_path_buf(),
            target: target.clone(),
            source,
        }
    })?;
    fs::rename(&temporary_path, link).map_err(|source| InstallationError::Activate {
        path: link.to_owned(),
        target,
        source,
    })?;
    temporary_path
        .keep()
        .map_err(|source| InstallationError::KeepSymlink {
            path: source.path.to_owned(),
            source: source.error,
        })?;
    Ok(())
}

/// Installation failures.
#[derive(Debug, thiserror::Error)]
pub enum InstallationError {
    /// A digest was not canonical SHA-256 text.
    #[error("invalid lowercase SHA-256 digest `{value}`")]
    InvalidDigest { value: String },
    /// Extraction bounds must be finite and nonzero.
    #[error("archive extraction limits must both be nonzero")]
    InvalidLimits,
    /// A path expected to be a directory was not one.
    #[error("path is not a real directory: {}", path.display())]
    NotDirectory { path: PathBuf },
    /// A required fixed release entry was absent.
    #[error("release {} is missing required entry `{name}`", root.display())]
    MissingReleaseEntry { root: PathBuf, name: &'static str },
    /// The fixed release root contained unknown content.
    #[error("unexpected deployment release entry: {}", path.display())]
    UnexpectedReleaseEntry { path: PathBuf },
    /// An entry was a link or special file.
    #[error("unsupported filesystem entry: {}", path.display())]
    UnsupportedFilesystemEntry { path: PathBuf },
    /// A supervisor lacked an executable bit.
    #[error("release supervisor is not executable: {}", path.display())]
    NotExecutable { path: PathBuf },
    /// A directory entry could not be represented as UTF-8.
    #[error("release entry does not have a UTF-8 name: {}", path.display())]
    NonUtf8Entry { path: PathBuf },
    /// Filesystem metadata could not be read.
    #[error("failed to inspect {}: {source}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A directory could not be read.
    #[error("failed to read directory {}: {source}", path.display())]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A directory could not be created.
    #[error("failed to create directory {}: {source}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A release staging directory could not be created.
    #[error("failed to create staging area in {}: {source}", path.display())]
    CreateStaging {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A regular file could not be read.
    #[error("failed to read {}: {source}", path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A regular file could not be written.
    #[error("failed to write {}: {source}", path.display())]
    WriteFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The manifest could not be read.
    #[error("failed to read bundle manifest {}: {source}", path.display())]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The manifest was not valid JSON.
    #[error("bundle manifest {} is not valid JSON: {source}", path.display())]
    InvalidManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The compiled manifest root was not a JSON object.
    #[error("bundle manifest {} must be a JSON object", path.display())]
    ManifestNotObject { path: PathBuf },
    /// An archive could not be created.
    #[error("failed to create archive {}: {source}", path.display())]
    CreateArchive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Archive writing failed.
    #[error("failed to write archive from {}: {source}", path.display())]
    WriteArchive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Archive reading failed.
    #[error("failed to read archive {}: {source}", path.display())]
    ReadArchive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// An archive path attempted to escape extraction.
    #[error("archive path is not safe and relative: {}", path.display())]
    UnsafeArchivePath { path: PathBuf },
    /// An archive repeated one canonical path.
    #[error("archive contains duplicate path: {}", path.display())]
    DuplicateArchivePath { path: PathBuf },
    /// An archive contained a link or special entry.
    #[error("unsupported archive entry: {}", path.display())]
    UnsupportedArchiveEntry { path: PathBuf },
    /// An extracted file did not match its declared size.
    #[error("archive entry {} declared {expected} bytes but produced {actual}", path.display())]
    ArchiveSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    /// Extraction exceeded caller-selected resource limits.
    #[error("archive exceeds extraction limits: {files} files and {bytes} bytes, limits are {} files and {} bytes", limits.files, limits.bytes)]
    ExtractionLimit {
        files: u64,
        bytes: u64,
        limits: ExtractionLimits,
    },
    /// A checksum sidecar could not be read.
    #[error("failed to read checksum {}: {source}", path.display())]
    ReadChecksum {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A checksum sidecar could not be written.
    #[error("failed to write checksum {}: {source}", path.display())]
    WriteChecksum {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Archive bytes did not match their sidecar.
    #[error("archive checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    /// A staged release could not be published atomically.
    #[error("failed to publish staged release {} as {}: {source}", from.display(), to.display())]
    PublishRelease {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A raced staging directory could not be removed.
    #[error("failed to clean staged release {}: {source}", path.display())]
    CleanupStaging {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The active symlink could not be read.
    #[error("failed to read active release link {}: {source}", path.display())]
    ReadActive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The active symlink pointed outside the immutable release store.
    #[error("active release has invalid target {}", target.display())]
    InvalidActiveTarget { target: PathBuf },
    /// The active path had no parent.
    #[error("active release path has no parent: {}", path.display())]
    InvalidActivePath { path: PathBuf },
    /// A temporary activation link could not be removed.
    #[error("failed to remove temporary activation file {}: {source}", path.display())]
    RemoveTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A temporary symlink could not be created.
    #[error("failed to create symlink {} -> {}: {source}", path.display(), target.display())]
    CreateSymlink {
        path: PathBuf,
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The active link could not be atomically replaced.
    #[error("failed to activate {} at {}: {source}", target.display(), path.display())]
    Activate {
        path: PathBuf,
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The active release selector could not be removed.
    #[error("failed to deactivate active release link {}: {source}", path.display())]
    Deactivate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Temp-path bookkeeping failed after a successful rename.
    #[error("failed to retain activation link {}: {source}", path.display())]
    KeepSymlink {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(root: &Path, marker: &str) {
        fs::create_dir_all(root.join(BUNDLE_DIR).join("bin")).expect("bundle directories");
        fs::write(
            root.join(BUNDLE_DIR).join(MANIFEST_FILE),
            format!(r#"{{"marker":"{marker}"}}"#),
        )
        .expect("manifest");
        fs::write(root.join(BUNDLE_DIR).join("bin").join("brain"), b"brain").expect("brain");
        fs::write(root.join(SUPERVISOR_FILE), b"supervisor").expect("supervisor");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.join(SUPERVISOR_FILE),
                fs::Permissions::from_mode(0o755),
            )
            .expect("executable supervisor");
            fs::set_permissions(
                root.join(BUNDLE_DIR).join("bin").join("brain"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("executable brain");
        }
    }

    #[test]
    fn deterministic_archive_installs_and_activates_by_digest() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let source = temp.path().join("source");
        fs::create_dir(&source).expect("source");
        release(&source, "first");
        let first_archive = temp.path().join("first.build.phoxal");
        let first_checksum = temp.path().join("first.sha256");
        let first = create_archive(&source, &first_archive, &first_checksum).expect("archive");
        let second_archive = temp.path().join("second.build.phoxal");
        let second_checksum = temp.path().join("second.sha256");
        let second = create_archive(&source, &second_archive, &second_checksum).expect("archive");
        assert_eq!(first, second);
        assert_eq!(
            fs::read(&first_archive).expect("first bytes"),
            fs::read(&second_archive).expect("second bytes")
        );

        let installation = Installation::open(temp.path().join("installation")).expect("store");
        let installed = installation
            .install(&first_archive, &first_checksum)
            .expect("install");
        assert_eq!(installed.id, first);
        let activation = installation.activate(&first).expect("activate");
        assert_eq!(activation.previous, None);
        assert_eq!(installation.active_id().expect("active id"), Some(first));
    }

    #[test]
    fn explicit_rollback_restores_the_previous_release() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let installation = Installation::open(temp.path().join("installation")).expect("store");
        let mut ids = Vec::new();
        for marker in ["first", "second"] {
            let source = temp.path().join(marker);
            fs::create_dir(&source).expect("source");
            release(&source, marker);
            let archive = temp.path().join(format!("{marker}.build.phoxal"));
            let checksum = temp.path().join(format!("{marker}.sha256"));
            let id = create_archive(&source, &archive, &checksum).expect("archive");
            installation.install(&archive, &checksum).expect("install");
            ids.push(id);
        }
        installation.activate(&ids[0]).expect("first activation");
        let second = installation.activate(&ids[1]).expect("second activation");
        assert_eq!(second.previous, Some(ids[0].clone()));
        installation.rollback(&ids[0]).expect("rollback");
        assert_eq!(
            installation.active_id().expect("active"),
            Some(ids[0].clone())
        );
        assert_eq!(
            fs::read_to_string(
                installation
                    .release_path(&ids[0])
                    .join(BUNDLE_DIR)
                    .join(MANIFEST_FILE)
            )
            .expect("manifest"),
            r#"{"marker":"first"}"#
        );
    }

    #[test]
    fn checksum_mismatch_and_unsafe_archives_are_refused_before_publication() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let source = temp.path().join("source");
        fs::create_dir(&source).expect("source");
        release(&source, "first");
        let archive = temp.path().join("build.phoxal");
        let checksum = temp.path().join("build.phoxal.sha256");
        create_archive(&source, &archive, &checksum).expect("archive");
        fs::write(&checksum, format!("{}\n", "0".repeat(64))).expect("wrong checksum");
        let installation = Installation::open(temp.path().join("installation")).expect("store");
        assert!(matches!(
            installation.install(&archive, &checksum),
            Err(InstallationError::ChecksumMismatch { .. })
        ));

        let malicious = temp.path().join("malicious.build.phoxal");
        let file = fs::File::create(&malicious).expect("malicious archive");
        let encoder = GzEncoder::new(file, Compression::fast());
        let mut builder = Builder::new(encoder);
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_data(&mut header, "escape", io::empty())
            .expect("symlink entry");
        let encoder = builder.into_inner().expect("tar");
        encoder.finish().expect("gzip");
        let malicious_checksum = temp.path().join("malicious.sha256");
        fs::write(
            &malicious_checksum,
            format!("{}\n", digest_file(&malicious).expect("digest")),
        )
        .expect("checksum");
        assert!(matches!(
            installation.install(&malicious, &malicious_checksum),
            Err(InstallationError::UnsupportedArchiveEntry { .. })
        ));
    }

    #[test]
    fn extraction_limits_are_enforced_before_layout_publication() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let source = temp.path().join("source");
        fs::create_dir(&source).expect("source");
        release(&source, "first");
        let archive = temp.path().join("build.phoxal");
        let checksum = temp.path().join("build.phoxal.sha256");
        create_archive(&source, &archive, &checksum).expect("archive");
        let installation = Installation::with_limits(
            temp.path().join("installation"),
            ExtractionLimits {
                files: 1,
                bytes: 16,
            },
        )
        .expect("store");
        assert!(matches!(
            installation.install(&archive, &checksum),
            Err(InstallationError::ExtractionLimit { .. })
        ));
        assert!(
            fs::read_dir(temp.path().join("installation").join(RELEASES_DIR))
                .expect("releases")
                .all(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".install-"))
        );
    }
}
