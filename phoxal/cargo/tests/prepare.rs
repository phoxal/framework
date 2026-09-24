use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

#[test]
fn exact_local_participant_installs_without_a_robot_cargo_dependency()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let robot = directory.path().join("robot");
    let provider = robot.join("provider");
    let home = directory.path().join("phoxal-home");
    let cargo_home = directory.path().join("cargo-home");
    fs::create_dir_all(robot.join("src"))?;
    fs::create_dir_all(provider.join("src"))?;
    fs::create_dir_all(provider.join("api"))?;
    fs::create_dir_all(&cargo_home)?;
    fs::write(
        robot.join("Cargo.toml"),
        "[package]\nname = \"proof-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(robot.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-robot }\nservices:\n  motion:\n    package: proof-provider\n    version: '0.1.0'\n    source: { path: provider }\n",
    )?;
    fs::write(
        provider.join("Cargo.toml"),
        "[package]\nname = \"proof-provider\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(provider.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        provider.join("api/motion.proto"),
        "syntax = \"proto3\"; package proof.motion.v1; message Status { bool stopped = 1; } service Motion { rpc Read(Status) returns (Status); }\n",
    )?;
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--manifest-path"])
        .arg(provider.join("Cargo.toml"))
        .env("CARGO_HOME", &cargo_home)
        .output()?;
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-phoxal"))
        .arg("prepare")
        .arg("--offline")
        .current_dir(&robot)
        .env("CARGO_HOME", &cargo_home)
        .env("PHOXAL_HOME", &home)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("prepared motion proof-provider 0.1.0")
    );
    assert!(!robot.join(".phoxal").exists());
    assert!(!fs::read_to_string(robot.join("Cargo.toml"))?.contains("proof-provider"));
    assert!(contains_file(
        &home.join("packages/local/proof-provider"),
        "proof-provider"
    )?);
    assert!(contains_file(
        &home.join("packages/local/proof-provider"),
        "Cargo.lock"
    )?);
    Ok(())
}

fn contains_file(root: &Path, name: &str) -> std::io::Result<bool> {
    if !root.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if contains_file(&path, name)? {
                return Ok(true);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[test]
fn exact_git_revision_prepares_alternate_binary_and_api() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let package = "proof-git-provider";
    let version = "0.1.0";
    let source = directory.path().join("source");
    let robot = directory.path().join("robot");
    let cargo_home = directory.path().join("cargo-home");
    let phoxal_home = directory.path().join("phoxal-home");
    fs::create_dir_all(source.join("src"))?;
    fs::create_dir_all(source.join("api"))?;
    fs::create_dir_all(&robot)?;
    fs::create_dir_all(&cargo_home)?;
    fs::write(
        source.join("Cargo.toml"),
        format!(
            "[package]\nname = {package:?}\nversion = {version:?}\nedition = \"2024\"\n[[bin]]\nname = \"provider-daemon\"\npath = \"src/daemon.rs\"\n"
        ),
    )?;
    fs::write(source.join("src/daemon.rs"), "fn main() {}\n")?;
    let proto = "syntax = \"proto3\"; package proof.git.v1; message Status { bool ready = 1; } service Provider { rpc Read(Status) returns (Status); }\n";
    fs::write(source.join("api/status.proto"), proto)?;
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--manifest-path"])
        .arg(source.join("Cargo.toml"))
        .env("CARGO_HOME", &cargo_home)
        .output()?;
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&source)
        .status()?;
    assert!(init.success());
    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=Proof",
            "-c",
            "user.email=proof@example.test",
            "add",
            ".",
        ])
        .current_dir(&source)
        .status()?;
    assert!(commit.success());
    let commit = Command::new("git")
        .args([
            "-c",
            "user.name=Proof",
            "-c",
            "user.email=proof@example.test",
            "commit",
            "--quiet",
            "-m",
            "proof",
        ])
        .current_dir(&source)
        .status()?;
    assert!(commit.success());
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&source)
        .output()?;
    assert!(revision.status.success());
    let revision = String::from_utf8(revision.stdout)?.trim().to_owned();
    fs::write(
        robot.join("Cargo.toml"),
        "[package]\nname = \"proof-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(
        robot.join("robot.yaml"),
        format!(
            "schema: phoxal/robot/v0\nrobot: {{ id: proof-robot }}\nservices:\n  provider:\n    package: {package}\n    version: '{version}'\n    binary: provider-daemon\n    source:\n      git: file://{}\n      rev: {revision}\n",
            source.display()
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-phoxal"))
        .arg("prepare")
        .current_dir(&robot)
        .env("CARGO_HOME", &cargo_home)
        .env("PHOXAL_HOME", &phoxal_home)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(
            robot
                .join(".phoxal/git")
                .join(package)
                .join(&revision)
                .join(version)
                .join("api/status.proto")
        )?,
        proto
    );
    assert!(contains_file(
        &phoxal_home.join("packages/git").join(package),
        "provider-daemon"
    )?);
    Ok(())
}

#[test]
fn exact_registry_participant_recovers_prepared_api_without_cargo_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let package = "proof-registry-provider";
    let version = "0.1.0";
    let source = directory.path().join("source");
    let registry = directory.path().join("registry");
    let robot = directory.path().join("robot");
    let cargo_home = directory.path().join("cargo-home");
    let phoxal_home = directory.path().join("phoxal-home");
    fs::create_dir_all(source.join("src"))?;
    fs::create_dir_all(source.join("api"))?;
    fs::create_dir_all(&robot)?;
    fs::create_dir_all(&cargo_home)?;
    fs::write(
        source.join("Cargo.toml"),
        format!("[package]\nname = {package:?}\nversion = {version:?}\nedition = \"2024\"\n"),
    )?;
    fs::write(source.join("src/main.rs"), "fn main() {}\n")?;
    let proto = "syntax = \"proto3\"; package proof.registry.v1; message Status { bool ready = 1; } service Provider { rpc Read(Status) returns (Status); }\n";
    fs::write(source.join("api/status.proto"), proto)?;
    fs::write(source.join("LICENSE"), "Test fixture only.\n")?;
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--manifest-path"])
        .arg(source.join("Cargo.toml"))
        .env("CARGO_HOME", &cargo_home)
        .output()?;
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );

    let archive_dir = registry.join("api/v1/crates").join(package).join(version);
    fs::create_dir_all(&archive_dir)?;
    let archive_path = archive_dir.join("download");
    let archive = GzEncoder::new(fs::File::create(&archive_path)?, Compression::default());
    let mut archive = tar::Builder::new(archive);
    archive.append_dir_all(format!("{package}-{version}"), &source)?;
    archive.into_inner()?.finish()?;
    let checksum = Sha256::digest(fs::read(&archive_path)?);
    let checksum = checksum
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let index = registry.join("pr/oo");
    fs::create_dir_all(&index)?;
    fs::write(
        index.join(package),
        format!(
            "{}\n",
            serde_json::json!({"name": package, "vers": version, "deps": [], "cksum": checksum, "features": {}, "yanked": false})
        ),
    )?;

    let server = RegistryServer::start(&registry)?;
    fs::write(
        registry.join("config.json"),
        format!(
            "{{\"dl\":\"http://127.0.0.1:{}/api/v1/crates\"}}",
            server.port
        ),
    )?;
    fs::write(
        robot.join("Cargo.toml"),
        "[package]\nname = \"proof-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(
        robot.join("robot.yaml"),
        format!(
            "schema: phoxal/robot/v0\nrobot: {{ id: proof-robot }}\nservices:\n  provider:\n    package: {package}\n    version: '{version}'\n    source: {{ registry: proof }}\n"
        ),
    )?;
    let prepared = robot
        .join(".phoxal/registry/proof")
        .join(package)
        .join(version)
        .join("api/status.proto");
    let prepare = || -> Result<std::process::Output, Box<dyn std::error::Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cargo-phoxal"))
            .arg("prepare")
            .current_dir(&robot)
            .env("CARGO_HOME", &cargo_home)
            .env("PHOXAL_HOME", &phoxal_home)
            .env(
                "CARGO_REGISTRIES_PROOF_INDEX",
                format!("sparse+http://127.0.0.1:{}/", server.port),
            )
            .output()?)
    };
    let first = prepare()?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(fs::read_to_string(&prepared)?, proto);
    assert!(contains_file(
        &phoxal_home.join("packages/registry/proof"),
        package
    )?);
    let warm = prepare()?;
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        warm.stderr.is_empty(),
        "warm preparation should reuse the exact package"
    );

    fs::remove_dir_all(robot.join(".phoxal"))?;
    fs::remove_dir_all(cargo_home.join("registry"))?;
    let recovered = prepare()?;
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert_eq!(fs::read_to_string(&prepared)?, proto);

    fs::write(&archive_path, b"tampered archive")?;
    fs::remove_dir_all(robot.join(".phoxal"))?;
    fs::remove_dir_all(cargo_home.join("registry"))?;
    let rejected = prepare()?;
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("checksum"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    Ok(())
}

#[test]
fn registry_update_selects_exact_candidate_and_keeps_previous_selection_on_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let package = "proof-registry-update";
    let registry = directory.path().join("registry");
    let robot = directory.path().join("robot");
    let cargo_home = directory.path().join("cargo-home");
    let phoxal_home = directory.path().join("phoxal-home");
    fs::create_dir_all(&robot)?;
    fs::create_dir_all(&cargo_home)?;
    let server = RegistryServer::start(&registry)?;
    fs::create_dir_all(&registry)?;
    fs::write(
        registry.join("config.json"),
        format!(
            "{{\"dl\":\"http://127.0.0.1:{}/api/v1/crates\"}}",
            server.port
        ),
    )?;
    let first = archive_update_candidate(
        directory.path(),
        &registry,
        &cargo_home,
        package,
        "0.1.0",
        true,
    )?;
    let second = archive_update_candidate(
        directory.path(),
        &registry,
        &cargo_home,
        package,
        "0.2.0",
        true,
    )?;
    let index = registry.join("pr/oo").join(package);
    fs::create_dir_all(index.parent().expect("index parent"))?;
    fs::write(&index, format!("{first}\n{second}\n"))?;
    fs::write(
        robot.join("Cargo.toml"),
        "[package]\nname = \"update-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(
        robot.join("robot.yaml"),
        format!(
            "schema: phoxal/robot/v0\nrobot:\n  id: update-robot\n  components:\n    sensor:\n      package: {package}\n      version: '0.1.0'\n      source: {{ registry: proof }}\n      mount_site: proof_mount\n      driver: {{}}\nservices:\n  provider:\n    package: {package}\n    version: '0.1.0'\n    source: {{ registry: proof }}\n"
        ),
    )?;
    let original = fs::read(robot.join("robot.yaml"))?;
    let run = |arguments: &[&str]| -> Result<std::process::Output, Box<dyn std::error::Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cargo-phoxal"))
            .args(arguments)
            .current_dir(&robot)
            .env("CARGO_HOME", &cargo_home)
            .env("PHOXAL_HOME", &phoxal_home)
            .env(
                "CARGO_REGISTRIES_PROOF_INDEX",
                format!("sparse+http://127.0.0.1:{}/", server.port),
            )
            .output()?)
    };
    let prepared = run(&["prepare"])?;
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let dry_run = run(&["update", "--dry-run", "service", "provider"])?;
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&dry_run.stdout).contains("0.1.0 -> 0.2.0"),
        "{}",
        String::from_utf8_lossy(&dry_run.stdout)
    );
    assert_eq!(fs::read(robot.join("robot.yaml"))?, original);
    assert!(
        !robot
            .join(".phoxal/registry/proof")
            .join(package)
            .join("0.2.0")
            .exists()
    );
    let component_dry_run = run(&["update", "--dry-run", "component", "sensor"])?;
    assert!(
        component_dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&component_dry_run.stderr)
    );
    assert!(String::from_utf8_lossy(&component_dry_run.stdout).contains("0.1.0 -> 0.2.0"));
    assert_eq!(fs::read(robot.join("robot.yaml"))?, original);
    let component_applied = run(&["update", "component", "sensor"])?;
    assert!(
        component_applied.status.success(),
        "{}",
        String::from_utf8_lossy(&component_applied.stderr)
    );
    let component_selected = fs::read_to_string(robot.join("robot.yaml"))?;
    let component_document: serde_yaml::Value = serde_yaml::from_str(&component_selected)?;
    assert_eq!(
        component_document["robot"]["components"]["sensor"]["version"],
        "0.2.0"
    );
    assert_eq!(
        component_document["services"]["provider"]["version"],
        "0.1.0"
    );

    let applied = run(&["update"])?;
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let selected = fs::read(robot.join("robot.yaml"))?;
    let selected_document: serde_yaml::Value = serde_yaml::from_slice(&selected)?;
    assert_eq!(
        selected_document["robot"]["components"]["sensor"]["version"],
        "0.2.0"
    );
    assert_eq!(
        selected_document["services"]["provider"]["version"],
        "0.2.0"
    );
    assert!(
        robot
            .join(".phoxal/registry/proof")
            .join(package)
            .join("0.2.0/api/status.proto")
            .is_file()
    );
    let no_op = run(&["update"])?;
    assert!(
        no_op.status.success(),
        "{}",
        String::from_utf8_lossy(&no_op.stderr)
    );
    assert_eq!(fs::read(robot.join("robot.yaml"))?, selected);

    let third = archive_update_candidate(
        directory.path(),
        &registry,
        &cargo_home,
        package,
        "0.3.0",
        false,
    )?;
    fs::write(&index, format!("{first}\n{second}\n{third}\n"))?;
    let rejected = run(&["update"])?;
    assert!(!rejected.status.success());
    assert_eq!(fs::read(robot.join("robot.yaml"))?, selected);
    assert!(
        robot
            .join(".phoxal/registry/proof")
            .join(package)
            .join("0.2.0/api/status.proto")
            .is_file()
    );
    Ok(())
}

fn archive_update_candidate(
    root: &Path,
    registry: &Path,
    cargo_home: &Path,
    package: &str,
    version: &str,
    valid_api: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let source = root.join(format!("source-{version}"));
    fs::create_dir_all(source.join("src"))?;
    fs::create_dir_all(source.join("api"))?;
    fs::write(
        source.join("Cargo.toml"),
        format!("[package]\nname = {package:?}\nversion = {version:?}\nedition = \"2024\"\n"),
    )?;
    fs::write(source.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(source.join("build.rs"), "fn main() {}\n")?;
    let proto = if valid_api {
        "syntax = \"proto3\"; package proof.update.v1; message Status { bool ready = 1; } service Provider { rpc Read(Status) returns (Status); }\n"
    } else {
        "syntax = \"proto3\"; package proof.update.v1; message Status { bool ready = 1; } service Provider { rpc Read(Status) returns (Status); } service Other { rpc Read(Status) returns (Status); }\n"
    };
    fs::write(source.join("api/status.proto"), proto)?;
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--manifest-path"])
        .arg(source.join("Cargo.toml"))
        .env("CARGO_HOME", cargo_home)
        .output()?;
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let archive_path = registry
        .join("api/v1/crates")
        .join(package)
        .join(version)
        .join("download");
    fs::create_dir_all(registry.join("api/v1/crates").join(package).join(version))?;
    let archive = GzEncoder::new(fs::File::create(&archive_path)?, Compression::default());
    let mut archive = tar::Builder::new(archive);
    archive.append_dir_all(format!("{package}-{version}"), &source)?;
    archive.into_inner()?.finish()?;
    let checksum = format!("{:x}", Sha256::digest(fs::read(&archive_path)?));
    Ok(serde_json::json!({
        "name": package,
        "vers": version,
        "deps": [{"name":"phoxal", "req":format!("={}", phoxal_build::SDK_VERSION), "kind":"build", "optional":false}],
        "cksum": checksum,
        "features": {},
        "yanked": false
    }).to_string())
}

struct RegistryServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RegistryServer {
    fn start(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let root = root.to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 4096];
                        let count = stream.read(&mut request).unwrap_or(0);
                        let line = String::from_utf8_lossy(&request[..count]);
                        let path = line.split_whitespace().nth(1).unwrap_or("/");
                        let path = path.split('?').next().unwrap_or("/");
                        let response = fs::read(root.join(path.trim_start_matches('/')));
                        let (status, body) = match response {
                            Ok(body) => ("200 OK", body),
                            Err(_) => ("404 Not Found", Vec::new()),
                        };
                        let header = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for RegistryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
