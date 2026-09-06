//! Webots installation validation and owned native process lifecycle.

use std::io::{Read, Write};
use std::num::NonZeroI32;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
pub use phoxal::world::api::session::document::NativeProcessIdentity;

const SUPPORTED_WEBOTS_VERSION: &str = "R2025a";
const GRACEFUL_BUDGET: Duration = Duration::from_secs(20);
const KILL_BUDGET: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One validated local Webots installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebotsInstallation {
    home: PathBuf,
    executable: PathBuf,
    version: String,
}

impl WebotsInstallation {
    /// Discover Webots R2025a from `WEBOTS_HOME` or its platform default.
    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("WEBOTS_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(default_webots_home);
        Self::at(home)
    }

    /// Validate one explicit Webots home.
    pub fn at(home: PathBuf) -> Result<Self> {
        let executable = executable_in(&home);
        ensure!(
            executable.is_file(),
            "Webots executable is missing at {}; install Webots {} or set WEBOTS_HOME",
            executable.display(),
            SUPPORTED_WEBOTS_VERSION
        );
        let output = Command::new(&executable)
            .arg("--version")
            .output()
            .with_context(|| format!("failed to run {} --version", executable.display()))?;
        ensure!(
            output.status.success(),
            "{} --version failed with {}",
            executable.display(),
            output.status
        );
        let text =
            String::from_utf8(output.stdout).context("Webots version output is not UTF-8")?;
        let version = parse_version(&text)?;
        ensure!(
            version == SUPPORTED_WEBOTS_VERSION,
            "unsupported Webots version {version}; this framework train requires {SUPPORTED_WEBOTS_VERSION}"
        );
        Ok(Self {
            home,
            executable,
            version: version.to_owned(),
        })
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// The Webots process tree owned by one world host.
#[derive(Debug)]
pub struct WebotsProcess {
    child: Child,
    executable: PathBuf,
    process_group: Option<NonZeroI32>,
    log: Arc<Mutex<LogState>>,
    readers: Vec<JoinHandle<()>>,
}

/// Kill-and-reap ownership installed immediately after `spawn` succeeds.
///
/// Launch still has to acquire both output pipes and start their reader threads.
/// Keeping the child in this guard until all of that setup succeeds prevents any
/// intermediate error from orphaning Webots or its process group.
struct SpawnGuard {
    child: Option<Child>,
    process_group: Option<NonZeroI32>,
}

#[derive(Debug)]
struct LogState {
    file: std::fs::File,
    limit: u64,
    written: u64,
    truncated: bool,
    error: Option<String>,
}

/// Final result of draining the bounded Webots output capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogCaptureOutcome {
    pub bytes: u64,
    pub truncated: bool,
}

impl WebotsProcess {
    pub fn identity(&self) -> Result<NativeProcessIdentity> {
        Ok(NativeProcessIdentity {
            process: crate::registration::process_identity(self.child.id())?,
            executable: self.executable.clone(),
            process_group: self
                .process_group
                .map(|group| u32::try_from(group.get()))
                .transpose()
                .context("Webots process group is negative")?,
        })
    }

    /// Launch one generated world in real time for its controller-owned bootstrap.
    ///
    /// The world controller immediately enters `PAUSE` before its first `wb_robot_step`, reports
    /// readiness, and stays outside that call while paused so host directives remain observable.
    pub fn launch(
        installation: &WebotsInstallation,
        world: &Path,
        log: &Path,
        log_byte_limit: u64,
        no_rendering: bool,
    ) -> Result<Self> {
        ensure!(
            world.is_file(),
            "generated Webots world is missing at {}",
            world.display()
        );
        let parent = log.parent().context("Webots log path has no parent")?;
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create Webots log directory {}", parent.display())
        })?;
        ensure!(log_byte_limit > 0, "Webots log byte limit must be positive");
        let output = owner_log_file(log)
            .with_context(|| format!("failed to create Webots log {}", log.display()))?;
        let log_state = Arc::new(Mutex::new(LogState {
            file: output,
            limit: log_byte_limit,
            written: 0,
            truncated: false,
            error: None,
        }));
        // Each native instance needs its own auxiliary Webots port even though
        // Phoxal never uses the external-controller or robot-window protocol.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .context("failed to select an available native Webots port")?
            .local_addr()?
            .port();
        let args = launch_args(world, no_rendering, port);
        let executable = installation
            .executable()
            .canonicalize()
            .context("failed to canonicalize the Webots executable")?;
        let mut command = Command::new(&executable);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().with_context(|| {
            format!(
                "failed to launch Webots {} for {}",
                installation.version(),
                world.display()
            )
        })?;
        let mut spawned = SpawnGuard::new(child);
        #[cfg(unix)]
        let process_group = Some(
            NonZeroI32::new(
                libc::pid_t::try_from(spawned.child()?.id())
                    .context("Webots process id does not fit in a process-group id")?,
            )
            .context("Webots process id must be positive")?,
        );
        #[cfg(not(unix))]
        let process_group = None;
        spawned.set_process_group(process_group);
        let stdout = spawned
            .child_mut()?
            .stdout
            .take()
            .context("Webots stdout pipe is missing")?;
        let stderr = spawned
            .child_mut()?
            .stderr
            .take()
            .context("Webots stderr pipe is missing")?;
        let readers = vec![
            spawn_log_reader("webots-stdout", stdout, Arc::clone(&log_state))?,
            spawn_log_reader("webots-stderr", stderr, Arc::clone(&log_state))?,
        ];
        let (child, process_group) = spawned.disarm()?;
        Ok(Self {
            child,
            executable,
            process_group,
            log: log_state,
            readers,
        })
    }

    /// Observe whether the direct Webots process has exited.
    pub fn exited(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Stop Webots gracefully, then kill only its owned process tree if needed.
    pub async fn stop(mut self) -> Result<LogCaptureOutcome> {
        #[cfg(unix)]
        {
            let process_group = self
                .process_group
                .context("Webots process group ownership was already released")?;
            for (signal, budget) in [
                (libc::SIGTERM, GRACEFUL_BUDGET),
                (libc::SIGKILL, KILL_BUDGET),
            ] {
                signal_process_group(process_group, signal)?;
                let deadline = tokio::time::Instant::now() + budget;
                loop {
                    if !process_group_alive(&mut self.child, process_group)? {
                        self.process_group = None;
                        return self.finish_log_capture();
                    }
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
                }
            }
            bail!("Webots process group remained alive after SIGKILL")
        }
        #[cfg(not(unix))]
        {
            self.child.kill().context("failed to stop Webots")?;
            self.child.wait().context("failed to reap Webots")?;
            self.finish_log_capture()
        }
    }

    fn finish_log_capture(&mut self) -> Result<LogCaptureOutcome> {
        for reader in self.readers.drain(..) {
            reader
                .join()
                .map_err(|_| anyhow::anyhow!("Webots log capture thread panicked"))?;
        }
        let state = lock(&self.log);
        if let Some(error) = &state.error {
            bail!("Webots log capture failed: {error}");
        }
        state
            .file
            .sync_all()
            .context("failed to persist Webots log")?;
        Ok(LogCaptureOutcome {
            bytes: state.written,
            truncated: state.truncated,
        })
    }
}

impl SpawnGuard {
    fn new(child: Child) -> Self {
        Self {
            child: Some(child),
            process_group: None,
        }
    }

    fn child(&self) -> Result<&Child> {
        self.child
            .as_ref()
            .context("Webots launch ownership was already released")
    }

    fn child_mut(&mut self) -> Result<&mut Child> {
        self.child
            .as_mut()
            .context("Webots launch ownership was already released")
    }

    fn set_process_group(&mut self, process_group: Option<NonZeroI32>) {
        self.process_group = process_group;
    }

    fn disarm(mut self) -> Result<(Child, Option<NonZeroI32>)> {
        let child = self
            .child
            .take()
            .context("Webots launch ownership was already released")?;
        Ok((child, self.process_group.take()))
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group.take() {
            let _ = signal_process_group(process_group, libc::SIGKILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for WebotsProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group.take() {
            let _ = signal_process_group(process_group, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn spawn_log_reader(
    name: &str,
    reader: impl std::io::Read + Send + 'static,
    state: Arc<Mutex<LogState>>,
) -> Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || capture_log(reader, &state))?)
}

fn capture_log(mut reader: impl Read, state: &Arc<Mutex<LogState>>) {
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes = match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(bytes) => bytes,
            Err(error) => {
                lock(state).error = Some(error.to_string());
                return;
            }
        };
        let mut state = lock(state);
        let remaining = state.limit.saturating_sub(state.written);
        let retained = usize::try_from(remaining.min(bytes as u64)).unwrap_or(bytes);
        if retained > 0 {
            if let Err(error) = state.file.write_all(&buffer[..retained]) {
                state.error = Some(error.to_string());
                return;
            }
            state.written = state.written.saturating_add(retained as u64);
        }
        if retained < bytes {
            state.truncated = true;
        }
    }
}

fn owner_log_file(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn default_webots_home() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Applications/Webots.app")
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/usr/local/webots")
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"C:\Program Files\Webots")
    }
}

fn executable_in(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Contents/MacOS/webots")
    }
    #[cfg(target_os = "linux")]
    {
        home.join("webots")
    }
    #[cfg(target_os = "windows")]
    {
        home.join("msys64/mingw64/bin/webots.exe")
    }
}

fn parse_version(output: &str) -> Result<&str> {
    output
        .split_ascii_whitespace()
        .find(|word| word.starts_with('R'))
        .context("Webots --version output did not contain an R-prefixed release")
}

fn launch_args(world: &Path, no_rendering: bool, port: u16) -> Vec<String> {
    let mut args = vec![
        "--mode=realtime".to_owned(),
        "--batch".to_owned(),
        "--stdout".to_owned(),
        "--stderr".to_owned(),
        format!("--port={port}"),
    ];
    if no_rendering {
        args.push("--no-rendering".to_owned());
    }
    args.push(world.display().to_string());
    args
}

#[cfg(unix)]
fn signal_process_group(process_group: NonZeroI32, signal: libc::c_int) -> Result<()> {
    // SAFETY: `kill` takes no pointer and the negative id targets only the owned group.
    if unsafe { libc::kill(-process_group.get(), signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error).context("failed to signal the Webots process group")
}

#[cfg(unix)]
fn process_group_alive(child: &mut Child, process_group: NonZeroI32) -> Result<bool> {
    let _ = child.try_wait()?;
    // SAFETY: signal zero performs no mutation and the negative id selects the owned group.
    if unsafe { libc::kill(-process_group.get(), 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error).context("failed to inspect the Webots process group"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_supported_native_release_is_selected() {
        assert_eq!(
            parse_version("Webots version: R2025a\n").expect("the version parses"),
            SUPPORTED_WEBOTS_VERSION
        );
    }

    #[cfg(unix)]
    #[test]
    fn installation_validation_executes_and_rejects_an_unsupported_release() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary Webots home");
        let executable = executable_in(directory.path());
        std::fs::create_dir_all(executable.parent().expect("executable has a parent"))
            .expect("fake Webots executable directory");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' 'Webots version: R2024b'\n",
        )
        .expect("fake Webots executable");
        let mut permissions = std::fs::metadata(&executable)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).expect("fake executable permissions");

        let error = WebotsInstallation::at(directory.path().to_path_buf())
            .expect_err("an unsupported Webots release is rejected");
        assert_eq!(
            error.to_string(),
            "unsupported Webots version R2024b; this framework train requires R2025a"
        );
    }

    #[test]
    fn generated_launch_uses_real_time_only_for_bootstrap() {
        let args = launch_args(Path::new("/tmp/world.wbt"), true, 49152);
        assert_eq!(args[0], "--mode=realtime");
        assert!(args.iter().any(|arg| arg == "--port=49152"));
        assert!(args.iter().any(|arg| arg == "--no-rendering"));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("fast") || arg == "--mode=run")
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_post_spawn_setup_kills_and_reaps_the_owned_process_group() {
        use std::os::unix::process::CommandExt as _;

        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn().expect("the test child launches");
        let process_group = NonZeroI32::new(
            libc::pid_t::try_from(child.id()).expect("the child id fits a process group"),
        )
        .expect("the child id is positive");
        let mut spawned = SpawnGuard::new(child);
        spawned.set_process_group(Some(process_group));

        drop(spawned);

        // SAFETY: signal zero performs no mutation and the negative id selects the test group.
        let result = unsafe { libc::kill(-process_group.get(), 0) };
        assert_eq!(result, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
