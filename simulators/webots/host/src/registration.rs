//! Owner-only atomic local registration and host-held lease.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use phoxal::model::world::{WorldBundle, WorldInstanceId};
use phoxal::version::FrameworkVersion;
use phoxal::world::api::session::document::LOCAL_WORLD_REGISTRATION_SCHEMA;
pub use phoxal::world::api::session::document::{
    LocalWorldRegistration, ProcessIdentity, RegisteredWorld,
};
use sysinfo::{Pid, System};

pub const REGISTRY_DIRECTORY_ENV: &str = "PHOXAL_SIMULATION_REGISTRY_DIR";
pub const EVIDENCE_DIRECTORY_ENV: &str = "PHOXAL_SIMULATION_EVIDENCE_DIR";
pub const LOG_BYTE_LIMIT_ENV: &str = "PHOXAL_SIMULATION_LOG_BYTE_LIMIT";
/// A live registration whose adjacent lease remains exclusively locked.
pub struct RegistrationGuard {
    registration_path: PathBuf,
    lease_path: PathBuf,
    lease: Option<File>,
    document: LocalWorldRegistration,
}

impl RegistrationGuard {
    /// Atomically publish one new registration while retaining its exclusive lease.
    pub fn create(
        root: impl AsRef<Path>,
        instance: WorldInstanceId,
        endpoint: String,
        bundle: &WorldBundle,
        process: ProcessIdentity,
    ) -> Result<Self> {
        let root = secure_directory(root.as_ref())?;
        let lease_name = format!("{instance}.lease");
        let lease_path = root.join(&lease_name);
        let registration_path = root.join(format!("{instance}.json"));
        let mut lease = owner_file(&lease_path)?;
        lock_exclusive(&lease)
            .with_context(|| format!("failed to lock world lease {}", lease_path.display()))?;
        lease
            .write_all(instance.to_string().as_bytes())
            .context("failed to initialize the world lease")?;
        lease
            .sync_all()
            .context("failed to persist the world lease")?;

        let document = LocalWorldRegistration {
            schema: LOCAL_WORLD_REGISTRATION_SCHEMA.to_owned(),
            instance,
            endpoint,
            process,
            framework: FrameworkVersion::CURRENT,
            world: RegisteredWorld {
                id: bundle.world().id().clone(),
                digest: bundle.digest(),
            },
            lease: lease_name,
        };
        document.validate_structure(instance)?;
        let body = serde_json::to_vec(&document)?;
        atomic_owner_write(&root, &registration_path, &body)?;
        Ok(Self {
            registration_path,
            lease_path,
            lease: Some(lease),
            document,
        })
    }

    #[must_use]
    pub const fn document(&self) -> &LocalWorldRegistration {
        &self.document
    }
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.registration_path);
        if let Some(lease) = self.lease.take() {
            unlock(&lease);
            drop(lease);
        }
        let _ = std::fs::remove_file(&self.lease_path);
    }
}

pub fn current_process_identity() -> Result<ProcessIdentity> {
    process_identity(std::process::id())
}

pub(crate) fn process_identity(pid: u32) -> Result<ProcessIdentity> {
    let mut system = System::new();
    system.refresh_processes(
        sysinfo::ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
    );
    let process = system
        .process(Pid::from_u32(pid))
        .context("current process is absent from the host process table")?;
    Ok(ProcessIdentity {
        pid,
        started_at_unix_s: process.start_time(),
    })
}

fn secure_directory(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to open local registry directory {}", path.display()))?;
    let metadata = std::fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_dir(),
        "local registry root is not a directory: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` has no pointer arguments or side effects.
        let owner = unsafe { libc::geteuid() };
        ensure!(
            metadata.uid() == owner,
            "local registry root is owned by another user"
        );
        ensure!(
            metadata.mode() & 0o077 == 0,
            "local registry root must have mode 0700 or stricter"
        );
    }
    Ok(path)
}

fn owner_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("failed to create owner-only file {}", path.display()))
}

fn atomic_owner_write(root: &Path, target: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = root.join(format!(
        ".registration-{}-{}.tmp",
        std::process::id(),
        target
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("world")
    ));
    let outcome = (|| -> Result<()> {
        let mut file = owner_file(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, target).with_context(|| {
            format!(
                "failed to atomically publish registration {}",
                target.display()
            )
        })?;
        File::open(root)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: this locks the valid descriptor borrowed from `file` and retains the file in guard.
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "world lease is already held: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> Result<()> {
    anyhow::bail!("local world leases are not implemented on this platform")
}

#[cfg(unix)]
fn unlock(file: &File) {
    use std::os::fd::AsRawFd as _;
    // SAFETY: the descriptor belongs to this guard; unlock occurs before it is dropped.
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(not(unix))]
fn unlock(_file: &File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::model::world::WorldDigest;

    fn primitive_bundle(root: &Path) -> WorldBundle {
        let source = root.join("bundle");
        std::fs::create_dir(&source).expect("world bundle directory");
        std::fs::create_dir(source.join("assets")).expect("world bundle assets");
        let document = source.join("world.json");
        std::fs::write(
            &document,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": "phoxal/world-bundle/v0",
                "id": "warehouse",
                "time_step_ns": 12_000_000,
                "gravity_mps2": [0.0, 0.0, -9.81],
                "spawn_points": {},
                "entities": [{
                    "declaration": "floor",
                    "instance": 0,
                    "pose": { "xyz": [0.0, 0.0, -0.05], "rpy": [0.0, 0.0, 0.0] },
                    "geometry": { "kind": "box", "size": [10.0, 10.0, 0.1] },
                    "collision": { "kind": "box", "size": [10.0, 10.0, 0.1] }
                }]
            }))
            .expect("world document JSON"),
        )
        .expect("world bundle document");
        WorldBundle::open(source).expect("primitive world bundle")
    }

    #[test]
    fn registration_wire_shape_is_pinned() {
        let instance =
            WorldInstanceId::parse("10000000000000000000000000000001").expect("canonical instance");
        let document = LocalWorldRegistration {
            schema: LOCAL_WORLD_REGISTRATION_SCHEMA.to_owned(),
            instance,
            endpoint: "tcp://127.0.0.1:1234".to_owned(),
            process: ProcessIdentity {
                pid: 42,
                started_at_unix_s: 99,
            },
            framework: "0.68.0".parse().expect("canonical framework version"),
            world: RegisteredWorld {
                id: "warehouse".parse().expect("canonical world id"),
                digest: WorldDigest::parse(&"aa".repeat(32)).expect("canonical digest"),
            },
            lease: format!("{instance}.lease"),
        };
        let value = serde_json::to_value(document).expect("registration encodes");
        assert_eq!(value["schema"], LOCAL_WORLD_REGISTRATION_SCHEMA);
        assert_eq!(value["process"]["started_at_unix_s"], 99);
        assert!(value.get("controller_endpoint").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn registration_is_owner_only_atomically_visible_and_lease_guarded() {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let temporary = tempfile::tempdir().expect("temporary root");
        let registry = temporary.path().join("registry");
        std::fs::create_dir(&registry).expect("registry directory");
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o700))
            .expect("owner-only registry");
        let bundle = primitive_bundle(temporary.path());
        let instance =
            WorldInstanceId::parse("10000000000000000000000000000001").expect("instance");
        let guard = RegistrationGuard::create(
            &registry,
            instance,
            "tcp://127.0.0.1:1234".to_owned(),
            &bundle,
            ProcessIdentity {
                pid: 42,
                started_at_unix_s: 99,
            },
        )
        .expect("registration");

        let registration_path = registry.join(format!("{instance}.json"));
        let lease_path = registry.join(format!("{instance}.lease"));
        for path in [&registration_path, &lease_path] {
            let metadata = std::fs::symlink_metadata(path).expect("published owner file");
            assert!(metadata.is_file());
            assert_eq!(metadata.mode() & 0o777, 0o600);
        }
        assert!(
            std::fs::read_dir(&registry)
                .expect("registry listing")
                .all(|entry| !entry
                    .expect("registry entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".registration-")),
            "the atomic staging file is never discoverable after publication"
        );
        let published: LocalWorldRegistration =
            serde_json::from_slice(&std::fs::read(&registration_path).expect("registration bytes"))
                .expect("registration JSON");
        assert_eq!(published, *guard.document());

        let competing = File::open(&lease_path).expect("competing lease descriptor");
        // SAFETY: `flock` acts on the valid descriptor retained by `competing`.
        let locked = unsafe { libc::flock(competing.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(locked, -1, "a live registration keeps its lease locked");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EWOULDBLOCK)
        );

        drop(guard);
        assert!(!registration_path.exists());
        assert!(!lease_path.exists());
    }
}
