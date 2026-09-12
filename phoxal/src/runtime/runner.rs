//! The bounded process runner for the synchronous [`super::Runtime`] contract.
//!
//! The runner owns the process lifecycle around a runtime owner.  It selects
//! one newest-due hardware release, freezes one input cut, executes the step
//! under its host deadline, reserves all output capacity, and only then
//! advances the schedule.  Input and output transport remain explicit host
//! implementations because only a contract owner knows how to encode its
//! generated payloads.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::core::{AcceptedInvocation, Config, OutputAdmission, RegisteredRuntime, RuntimeOwner};
use super::input::InputSnapshot;
use super::schedule::{HardwareInvocation, HardwareSchedule, ScheduleError};
use super::{ExecutionTime, RuntimeStatus};

/// The strict process arguments supplied to one Runtime binary.
///
/// The bundle root and instance identity are explicit so a process can never
/// select a sibling instance by package or executable name.  The connection is
/// also explicit; no environment fallback or source-tree lookup is permitted.
#[derive(Clone, Debug, Eq, PartialEq, Parser)]
#[command(
    name = "phoxal-runtime",
    about = "Run one admitted Phoxal Runtime instance.",
    long_about = None
)]
pub struct RuntimeLaunch {
    /// Installed immutable bundle directory.
    #[arg(long = "bundle-root", value_name = "PATH")]
    pub bundle_root: PathBuf,
    /// Runtime instance identity selected by the bundle graph.
    #[arg(long = "instance-id", value_name = "ID", value_parser = parse_identifier)]
    pub instance_id: String,
    /// Supervisor rendezvous endpoint.
    #[arg(
        long = "connect",
        value_name = "ENDPOINT",
        required = true,
        value_parser = parse_endpoint
    )]
    pub connect: String,
}

impl RuntimeLaunch {
    /// Parse the process argv without consulting process environment state.
    pub fn parse() -> crate::Result<Self> {
        Self::try_parse().map_err(anyhow::Error::from)
    }
}

/// The selected executable and configuration entry admitted from a bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeLaunchManifest {
    root: PathBuf,
    robot_id: String,
    instance_id: String,
    executable: PathBuf,
    config: Value,
}

impl RuntimeLaunchManifest {
    /// Open and admit one source-side `phoxal/bundle/v0` entry.
    pub fn open(root: impl AsRef<Path>, instance_id: &str) -> crate::Result<Self> {
        let root = root.as_ref().canonicalize().map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: root.as_ref().to_owned(),
                source,
            })
        })?;
        if !root.is_dir() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: "bundle root is not a directory".to_owned(),
            }));
        }
        let manifest_path = root.join("manifest.json");
        let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: manifest_path.clone(),
                source,
            })
        })?;
        if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: "manifest.json is not a regular file".to_owned(),
            }));
        }
        let bytes = read_bounded(&manifest_path, 16 * 1024 * 1024)?;
        let manifest =
            serde_json::from_slice::<SourceBundleManifest>(&bytes).map_err(|source| {
                anyhow::anyhow!(RunnerError::BundleJson {
                    path: manifest_path.clone(),
                    source,
                })
            })?;
        if manifest.schema != "phoxal/bundle/v0" {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!(
                    "unsupported bundle schema `{}`; expected `phoxal/bundle/v0`",
                    manifest.schema
                ),
            }));
        }
        parse_identifier(instance_id)
            .map_err(|message| anyhow::anyhow!(RunnerError::BundleInvalid { message }))?;
        let executable = manifest
            .executables
            .iter()
            .find(|entry| entry.instance == instance_id)
            .ok_or_else(|| {
                anyhow::anyhow!(RunnerError::UnknownInstance {
                    instance: instance_id.to_owned(),
                })
            })?;
        let relative = safe_relative_path(&executable.path)?;
        let executable_path = root.join(relative);
        let path_metadata = fs::symlink_metadata(&executable_path).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: executable_path.clone(),
                source,
            })
        })?;
        if !path_metadata.is_file() || path_metadata.file_type().is_symlink() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("executable for `{instance_id}` is not a regular file"),
            }));
        }
        let canonical_executable = executable_path.canonicalize().map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: executable_path.clone(),
                source,
            })
        })?;
        if !canonical_executable.starts_with(&root) {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("executable for `{instance_id}` resolves outside the bundle root"),
            }));
        }
        let metadata = fs::symlink_metadata(&canonical_executable).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: canonical_executable.clone(),
                source,
            })
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("executable for `{instance_id}` is not a regular file"),
            }));
        }
        verify_executable(&canonical_executable, executable)?;

        let config = if instance_id == "brain" {
            Value::Object(serde_json::Map::new())
        } else {
            manifest
                .document
                .services
                .get(instance_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(RunnerError::BundleInvalid {
                        message: format!(
                            "executable `{instance_id}` has no matching services entry"
                        ),
                    })
                })?
                .config
                .clone()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
        };
        if config.is_null() {
            return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
                message: format!("configuration for `{instance_id}` is explicit null"),
            }));
        }
        Ok(Self {
            root,
            robot_id: manifest.robot_id,
            instance_id: instance_id.to_owned(),
            executable: canonical_executable,
            config,
        })
    }

    /// Installed bundle root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compiled robot identity.
    #[must_use]
    pub fn robot_id(&self) -> &str {
        &self.robot_id
    }

    /// Admitted runtime instance identity.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Verified executable path for the selected instance.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Owned authored configuration value for typed decoding.
    #[must_use]
    pub fn config(&self) -> &Value {
        &self.config
    }

    /// Decode the selected configuration into the exact runtime type.
    pub fn decode_config<R: RegisteredRuntime>(&self) -> crate::Result<R::Config> {
        decode_config(self.config.clone())
    }
}

/// A source of one immutable input cut at the selected hardware boundary.
pub trait InputSource<R: RegisteredRuntime> {
    /// Freeze and return the complete input snapshot for one candidate.
    fn freeze(&mut self, candidate: &HardwareInvocation) -> crate::Result<R::Inputs>;

    /// Stop subscriptions, pending requests, and managed operation workers.
    fn stop(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Clear pending input work before a fresh execution initialization.
    fn reset(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

/// A sink that reserves and publishes one complete accepted output batch.
pub trait OutputSink<Outputs>: OutputAdmission<Outputs> {
    /// Publish an already-reserved complete invocation.
    fn publish(
        &mut self,
        accepted: AcceptedInvocation<Outputs, Self::Reservation>,
    ) -> crate::Result<()>;

    /// Stop publishers and wait for managed operation cleanup.
    fn stop(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Clear pending output work before a fresh execution initialization.
    fn reset(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

/// A host-monotonic clock used by [`RuntimeRunner::run_until_stop`].
pub trait RuntimeClock {
    /// Return elapsed host time from this clock's origin.
    fn now(&mut self) -> ExecutionTime;

    /// Wait until a logical release, returning early when the host is stopped.
    fn wait_until(&mut self, release: ExecutionTime) -> crate::Result<()>;
}

/// A system host clock with one monotonic origin.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Start a clock at the current host-monotonic instant.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeClock for SystemClock {
    fn now(&mut self) -> ExecutionTime {
        ExecutionTime::from(self.origin.elapsed())
    }

    fn wait_until(&mut self, release: ExecutionTime) -> crate::Result<()> {
        let target = Duration::from(release);
        if let Some(remaining) = target.checked_sub(self.origin.elapsed()) {
            std::thread::sleep(remaining);
        }
        Ok(())
    }
}

/// Result of one bounded runner poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollOutcome {
    /// The release was not due yet.
    NotDue { next_release: ExecutionTime },
    /// One complete invocation was accepted and published.
    Accepted { invocation_index: u64 },
    /// Stop was requested before selecting a candidate.
    Stopped,
}

/// A complete hardware runtime owner and its transport adapters.
pub struct RuntimeRunner<R, Inputs, Outputs>
where
    R: RegisteredRuntime,
    Inputs: InputSource<R>,
    Outputs: OutputSink<R::Outputs>,
{
    owner: RuntimeOwner<R>,
    schedule: HardwareSchedule,
    inputs: Inputs,
    outputs: Outputs,
    stopped: bool,
}

impl<R, Inputs, Outputs> RuntimeRunner<R, Inputs, Outputs>
where
    R: RegisteredRuntime,
    R::Inputs: InputSnapshot,
    Inputs: InputSource<R>,
    Outputs: OutputSink<R::Outputs>,
{
    /// Initialize one owner, validate its specification, and bind adapters.
    pub fn new(
        service: R,
        now: ExecutionTime,
        config: R::Config,
        inputs: Inputs,
        outputs: Outputs,
    ) -> crate::Result<Self> {
        let initialization_started = Instant::now();
        let owner = RuntimeOwner::new(service, now, config)?;
        if initialization_started.elapsed() > R::SPEC.init_timeout.as_duration() {
            return Err(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let schedule = HardwareSchedule::new(now, R::SPEC.period)
            .map_err(|error| anyhow::anyhow!(RunnerError::Schedule(error)))?;
        Ok(Self {
            owner,
            schedule,
            inputs,
            outputs,
            stopped: false,
        })
    }

    /// Poll one candidate at an explicit host input-freeze time.
    pub fn poll(&mut self, now: ExecutionTime) -> crate::Result<PollOutcome> {
        if self.stopped {
            return Ok(PollOutcome::Stopped);
        }
        let candidate = match self.schedule.candidate(now) {
            Ok(candidate) => candidate,
            Err(ScheduleError::NotDue { next_release }) => {
                return Ok(PollOutcome::NotDue { next_release });
            }
            Err(error) => return self.fail(anyhow::anyhow!(RunnerError::Schedule(error))),
        };
        let started = Instant::now();
        let inputs = match catch_adapter(|| self.inputs.freeze(&candidate)) {
            Ok(inputs) => inputs,
            Err(error) => return self.fail(error),
        };
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let accepted =
            match self
                .owner
                .accept_with(&candidate.context(), &inputs, &mut self.outputs)
            {
                Ok(accepted) => accepted,
                Err(error) => return self.fail(error),
            };
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        let invocation_index = accepted.invocation().index();
        if let Err(error) = catch_adapter(|| self.outputs.publish(accepted)) {
            return self.fail(error);
        }
        if started.elapsed() > R::SPEC.timeout.as_duration() {
            return self.fail(anyhow::anyhow!(super::InvocationError::DeadlineExceeded));
        }
        if let Err(error) = self.schedule.accept(candidate) {
            return self.fail(anyhow::anyhow!(RunnerError::Schedule(error)));
        }
        Ok(PollOutcome::Accepted { invocation_index })
    }

    /// Drive the process until a host stop or a terminal lifecycle error.
    pub fn run_until_stop<C: RuntimeClock>(&mut self, clock: &mut C) -> crate::Result<()> {
        while !self.stopped {
            match self.poll(clock.now())? {
                PollOutcome::NotDue { next_release } => clock.wait_until(next_release)?,
                PollOutcome::Accepted { .. } => {}
                PollOutcome::Stopped => break,
            }
        }
        Ok(())
    }

    /// Request normal stop and clean up all owned transport/operation work.
    pub fn stop(&mut self) -> crate::Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let input_result = catch_adapter(|| self.inputs.stop());
        let output_result = catch_adapter(|| self.outputs.stop());
        let result = input_result.and(output_result);
        if result.is_err() {
            self.owner.fail();
        }
        result
    }

    /// Reinitialize a fresh execution with an owned, possibly non-Clone config.
    pub fn reset(&mut self, now: ExecutionTime, config: R::Config) -> crate::Result<()> {
        let input_result = catch_adapter(|| self.inputs.reset());
        let output_result = catch_adapter(|| self.outputs.reset());
        if let Err(error) = input_result.and(output_result) {
            self.owner.fail();
            self.stopped = true;
            self.cleanup_after_failure();
            return Err(error);
        }
        if let Err(error) = self.owner.reset(now, config) {
            self.stopped = true;
            self.cleanup_after_failure();
            return Err(error);
        }
        self.schedule = HardwareSchedule::new(now, R::SPEC.period)
            .map_err(|error| anyhow::anyhow!(RunnerError::Schedule(error)))
            .map_err(|error| {
                self.owner.fail();
                self.stopped = true;
                self.cleanup_after_failure();
                error
            })?;
        self.stopped = false;
        Ok(())
    }

    /// Runtime lifecycle status.
    #[must_use]
    pub const fn status(&self) -> RuntimeStatus {
        self.owner.status()
    }

    /// Current next nominal release.
    #[must_use]
    pub const fn next_release(&self) -> ExecutionTime {
        self.schedule.next_release()
    }

    fn fail(&mut self, error: anyhow::Error) -> crate::Result<PollOutcome> {
        self.owner.fail();
        self.stopped = true;
        self.cleanup_after_failure();
        Err(error)
    }

    fn cleanup_after_failure(&mut self) {
        let _ = catch_adapter(|| self.inputs.stop());
        let _ = catch_adapter(|| self.outputs.stop());
    }
}

fn catch_adapter<T>(operation: impl FnOnce() -> crate::Result<T>) -> crate::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(super::InvocationError::Panicked)),
    }
}

/// Errors at the process/bundle runner boundary.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// A bundle filesystem operation failed.
    #[error("bundle I/O failed for {path}: {source}")]
    BundleIo {
        /// Affected path.
        path: PathBuf,
        /// Filesystem source error.
        #[source]
        source: std::io::Error,
    },
    /// The bundle manifest was not admissible.
    #[error("invalid runtime bundle: {message}")]
    BundleInvalid {
        /// Diagnostic detail.
        message: String,
    },
    /// The bundle manifest could not be decoded.
    #[error("cannot decode runtime bundle manifest {path}: {source}")]
    BundleJson {
        /// Manifest path.
        path: PathBuf,
        /// JSON source error.
        #[source]
        source: serde_json::Error,
    },
    /// The selected instance did not occur in the executable table.
    #[error("runtime instance `{instance}` is not admitted by the bundle")]
    UnknownInstance {
        /// Requested instance.
        instance: String,
    },
    /// A selected executable digest or size did not match the bundle record.
    #[error("runtime executable `{instance}` does not match its bundle digest")]
    ExecutableMismatch {
        /// Selected instance.
        instance: String,
    },
    /// A schedule candidate failed before acceptance.
    #[error("runtime schedule failed: {0}")]
    Schedule(#[from] ScheduleError),
    /// Generated contract bindings are required for process transport.
    #[error(
        "runtime instance `{instance}` cannot start on `{connect}` because generated typed input/output bindings are missing"
    )]
    TypedBindingsUnavailable {
        /// Selected runtime instance.
        instance: String,
        /// Explicit supervisor endpoint.
        connect: String,
    },
}

#[derive(Debug, Deserialize)]
struct SourceBundleManifest {
    schema: String,
    robot_id: String,
    document: SourceDocument,
    executables: Vec<SourceExecutable>,
}

#[derive(Debug, Deserialize, Default)]
struct SourceDocument {
    #[serde(default)]
    services: BTreeMap<String, SourceService>,
}

#[derive(Debug, Deserialize, Default)]
struct SourceService {
    #[serde(default)]
    config: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SourceExecutable {
    instance: String,
    path: String,
    bytes: u64,
    sha256: String,
}

fn decode_config<C: Config>(value: Value) -> crate::Result<C> {
    match serde_json::from_value::<C>(value.clone()) {
        Ok(config) => Ok(config),
        Err(first) if value.as_object().is_some_and(serde_json::Map::is_empty) => {
            serde_json::from_value(Value::Null).map_err(|second| {
                anyhow::anyhow!(
                    "runtime configuration is not valid for the selected implementation: {first}; empty-object/unit fallback also failed: {second}"
                )
            })
        }
        Err(error) => Err(anyhow::anyhow!(
            "runtime configuration is not valid for the selected implementation: {error}"
        )),
    }
}

fn verify_executable(path: &Path, expected: &SourceExecutable) -> crate::Result<()> {
    let mut file = fs::File::open(path).map_err(|source| {
        anyhow::anyhow!(RunnerError::BundleIo {
            path: path.to_owned(),
            source,
        })
    })?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: path.to_owned(),
                source,
            })
        })?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hasher.finalize());
    if bytes != expected.bytes || digest != expected.sha256 {
        return Err(anyhow::anyhow!(RunnerError::ExecutableMismatch {
            instance: expected.instance.clone(),
        }));
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum: usize) -> crate::Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(|source| {
        anyhow::anyhow!(RunnerError::BundleIo {
            path: path.to_owned(),
            source,
        })
    })?;
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| {
            anyhow::anyhow!(RunnerError::BundleIo {
                path: path.to_owned(),
                source,
            })
        })?;
    if bytes.len() > maximum {
        return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
            message: format!("manifest.json exceeds the {maximum} byte startup bound"),
        }));
    }
    Ok(bytes)
}

fn safe_relative_path(path: &str) -> crate::Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(anyhow::anyhow!(RunnerError::BundleInvalid {
            message: format!("executable path `{path:?}` is not bundle-relative"),
        }));
    }
    Ok(path.to_owned())
}

fn parse_identifier(value: &str) -> Result<String, String> {
    if (1..=64).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        Ok(value.to_owned())
    } else {
        Err("instance id must be 1-64 lowercase ASCII letters, digits, '-' or '_'".to_owned())
    }
}

fn parse_endpoint(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.trim() != value
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        Err("connect endpoint must be non-empty and contain no surrounding whitespace or control characters".to_owned())
    } else {
        Ok(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::runtime::{InitContext, Runtime, RuntimeSpec, StepContext};

    #[derive(Debug, Deserialize)]
    struct TestConfig {
        value: u64,
    }

    impl Config for TestConfig {
        const SCHEMA_JSON: &'static str = "{}";
    }

    #[derive(Default)]
    struct TestInputs;

    impl super::super::input::InputSet for TestInputs {
        const FIELDS: &'static [super::super::input::InputField] = &[];
    }

    impl InputSnapshot for TestInputs {
        fn empty() -> Self {
            Self
        }
    }

    struct TestOutputs {
        value: u64,
    }

    struct TestRuntime {
        validate: Arc<Mutex<Vec<&'static str>>>,
        fail_step: bool,
        step_delay: Duration,
    }

    impl Runtime for TestRuntime {
        type Config = TestConfig;
        type State = u64;
        type Inputs = TestInputs;
        type Outputs = TestOutputs;

        fn validate_config(config: &Self::Config) -> crate::Result<()> {
            Ok((config.value > 0)
                .then_some(())
                .ok_or_else(|| anyhow::anyhow!("value must be positive"))?)
        }

        fn init(&self, _ctx: &InitContext, config: Self::Config) -> crate::Result<Self::State> {
            self.validate.lock().expect("lock").push("init");
            Ok(config.value)
        }

        fn step(
            &self,
            _ctx: &StepContext,
            state: Self::State,
            _inputs: &Self::Inputs,
        ) -> crate::Result<(Self::State, Self::Outputs)> {
            self.validate.lock().expect("lock").push("step");
            std::thread::sleep(self.step_delay);
            if self.fail_step {
                Err(anyhow::anyhow!("step failed"))
            } else {
                Ok((state + 1, TestOutputs { value: state + 1 }))
            }
        }
    }

    impl RegisteredRuntime for TestRuntime {
        const SPEC: RuntimeSpec = RuntimeSpec::from_millis(10, 10, 100);
    }

    struct TestInputsSource;

    impl InputSource<TestRuntime> for TestInputsSource {
        fn freeze(&mut self, _candidate: &HardwareInvocation) -> crate::Result<TestInputs> {
            Ok(TestInputs)
        }
    }

    struct TestSink {
        reserve: bool,
        published: Vec<u64>,
        stopped: bool,
    }

    impl OutputAdmission<TestOutputs> for TestSink {
        type Reservation = u64;

        fn reserve(&mut self, outputs: &TestOutputs) -> crate::Result<Self::Reservation> {
            if self.reserve {
                Ok(outputs.value)
            } else {
                Err(anyhow::anyhow!("output capacity refused"))
            }
        }
    }

    impl OutputSink<TestOutputs> for TestSink {
        fn publish(
            &mut self,
            accepted: AcceptedInvocation<TestOutputs, Self::Reservation>,
        ) -> crate::Result<()> {
            self.published.push(accepted.outputs().value);
            Ok(())
        }

        fn stop(&mut self) -> crate::Result<()> {
            self.stopped = true;
            Ok(())
        }
    }

    #[test]
    fn launch_parser_requires_explicit_bundle_instance_and_endpoint() {
        let parsed = RuntimeLaunch::try_parse_from([
            "runtime",
            "--bundle-root",
            "/tmp/bundle",
            "--instance-id",
            "motion",
            "--connect",
            "tcp/127.0.0.1:7447",
        ])
        .expect("launch parses");
        assert_eq!(parsed.instance_id, "motion");
        assert!(RuntimeLaunch::try_parse_from(["runtime", "--bundle-root", "/tmp"]).is_err());
    }

    #[test]
    fn runner_reserves_outputs_before_publishing_and_advancing() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(matches!(
            runner.poll(ExecutionTime::default()),
            Ok(PollOutcome::Accepted {
                invocation_index: 0
            })
        ));
        assert_eq!(runner.status(), RuntimeStatus::Ready);
    }

    #[test]
    fn validation_runs_before_init_for_a_non_clone_configuration() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime {
            validate: Arc::clone(&order),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("valid non-clone config starts");
        assert_eq!(&*order.lock().expect("lock"), &["init"]);
    }

    #[test]
    fn validation_failure_never_calls_init_for_an_owned_non_clone_config() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let runtime = TestRuntime {
            validate: Arc::clone(&order),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        assert!(
            RuntimeRunner::new(
                runtime,
                ExecutionTime::default(),
                TestConfig { value: 0 },
                TestInputsSource,
                TestSink {
                    reserve: true,
                    published: Vec::new(),
                    stopped: false,
                },
            )
            .is_err()
        );
        assert!(order.lock().expect("lock").is_empty());
    }

    #[test]
    fn output_refusal_faults_and_stops_the_runner_before_schedule_commit() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: false,
            step_delay: Duration::ZERO,
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: false,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(runner.poll(ExecutionTime::default()).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
        assert_eq!(runner.next_release(), ExecutionTime::default());
    }

    #[test]
    fn process_step_failure_is_terminal_and_does_not_publish() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: true,
            step_delay: Duration::ZERO,
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(runner.poll(ExecutionTime::default()).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
    }

    #[test]
    fn invocation_deadline_faults_the_owner_without_publishing() {
        let runtime = TestRuntime {
            validate: Arc::new(Mutex::new(Vec::new())),
            fail_step: false,
            step_delay: Duration::from_millis(20),
        };
        let mut runner = RuntimeRunner::new(
            runtime,
            ExecutionTime::default(),
            TestConfig { value: 1 },
            TestInputsSource,
            TestSink {
                reserve: true,
                published: Vec::new(),
                stopped: false,
            },
        )
        .expect("runner initializes");
        assert!(runner.poll(ExecutionTime::default()).is_err());
        assert_eq!(runner.status(), RuntimeStatus::Failed);
    }
}
