//! Regressions for editing a prepared composition and re-preparing it.
//!
//! A robot's own recorded brain products and its participants' freshly
//! prepared products must never conflict: the recorded self products feed
//! only bindings, never definitions, so editing the brain's messages or
//! selecting a new participant revision prepares and checks green without
//! deleting any cache by hand.
#![cfg(feature = "e2e")]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The framework checkout supplying the `phoxal` path dependency.
fn framework_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn phoxal_dep(features: &str) -> String {
    format!(
        "phoxal = {{ path = {:?}, default-features = false, features = [{features}] }}\n",
        framework_root().join("phoxal")
    )
}

/// Runs the compiled `cargo-phoxal` binary with an isolated Phoxal home.
/// One stable dependency-build tree under the workspace's ignored
/// `target/` directory, shared with `prepared_selection`'s suites: every
/// nested fixture resolves the same workspace phoxal path dependency plus
/// the same crates.io resolution, so the first fixture in a cold run pays
/// the build once and every later fixture, suite, and run reuses it. No
/// per-process directories are created, so repeated runs cannot
/// accumulate retained trees; `cargo clean` reclaims the space; and
/// cargo's target-dir file lock keeps concurrent fixture builds correct.
/// Fixture sources still rebuild on their own edits, and the cold path
/// itself stays proven by a clean checkout's first run. Nested builds
/// carry no debugging value, so they run without incremental compilation
/// and with line-tables-only debug info to keep the tree small.
fn shared_target_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/suites")
        .join("composition")
}

fn invoke(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-phoxal"))
        .current_dir(cwd)
        .env("PHOXAL_HOME", cwd.join(".phoxal-home"))
        .env("CARGO_TARGET_DIR", shared_target_dir())
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "line-tables-only")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("spawn cargo-phoxal: {error}"))
}

fn report(output: &std::process::Output) -> String {
    format!(
        "--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const BUILD_RS: &str = "fn main() -> Result<(), phoxal::build::Error> {\n    phoxal::build::api(phoxal::build::BuildApiConfig::default())\n}\n";

/// One participant: a projected status output over an authored payload.
const PROVIDER_BIN: &str = r#"//! Provider: one projected output over an authored package payload.
use phoxal::contracts::Latest;

#[phoxal::message(package = "proof.cycle.v1")]
pub struct ProviderState {
    #[phoxal(tag = 1)]
    pub value: u64,
}

#[phoxal::endpoints]
pub struct ProviderApi {
    #[phoxal::output(projection = state, max_bytes = 512)]
    provider_status: Latest<ProviderState>,
}

pub struct Provider {
    beats: u64,
}

#[phoxal::runtime(contract = ProviderApi, period_ms = 20)]
impl Provider {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self { beats: 0 })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
        self.beats = self.beats.saturating_add(1);
        Ok(())
    }

    #[publish(provider_status)]
    fn provider_status(&self) -> ProviderState {
        ProviderState { value: self.beats }
    }
}

fn main() {}
"#;

/// The brain: consumes the participant's status through an identity
/// connection and serves an authored heartbeat, so the compiled brain
/// retains definitions of its own messages beside the participant's.
/// An empty `heartbeat_package` keeps the message private; otherwise the
/// message shares that package namespace with the participant's payload.
fn brain_main(payload: &str, heartbeat_package: &str) -> String {
    let message = if heartbeat_package.is_empty() {
        "#[phoxal::message]".to_owned()
    } else {
        format!("#[phoxal::message(package = \"{heartbeat_package}\")]")
    };
    let beat_value = if payload == "String" {
        "format!(\"{}\", self.beats)".to_owned()
    } else {
        "self.beats".to_owned()
    };
    format!(
        r#"//! Brain: binds the participant's status and serves an authored
//! heartbeat report.
phoxal::api!();

use phoxal::contracts::{{Empty, Latest, RequestReply}};
use phoxal::runtime::Context;

use crate::api::provider::ProviderState;

{message}
pub struct Heartbeat {{
    #[phoxal(tag = 1)]
    pub beats: {payload},
}}

#[phoxal::endpoints(package = "proof.cycle.brain.v1")]
pub struct BrainApi {{
    #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
    telemetry: Latest<ProviderState>,

    #[phoxal::operation(max_items = 4, max_bytes = 256)]
    read: RequestReply<Empty, Heartbeat>,
}}

pub struct Brain {{
    beats: u64,
}}

#[phoxal::runtime(contract = BrainApi, period_ms = 50)]
impl Brain {{
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {{
        Ok(Self {{ beats: 0 }})
    }}

    #[handle(read)]
    fn read(&mut self, _ctx: &mut Context<'_, Self>, _request: Empty) -> phoxal::Result<Heartbeat> {{
        Ok(Heartbeat {{ beats: {beat_value} }})
    }}

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {{
        let _ = ctx.telemetry().value();
        self.beats = self.beats.saturating_add(1);
        Ok(())
    }}
}}

fn main() -> phoxal::Result<()> {{
    phoxal::runtime::run::<Brain>()
}}
"#
    )
}

fn robot_manifest() -> String {
    format!(
        "[package]\nname = \"proof-cycle-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\nbuild = \"build.rs\"\n\
         [dependencies]\n{}\nphoxal-supervisor = {{ path = {:?} }}\n\
         [build-dependencies]\n{}",
        phoxal_dep("\"runtime\""),
        framework_root().join("supervisor"),
        phoxal_dep("\"build\"")
    )
}

#[test]
fn brain_message_edit_keeps_preparation_green() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = root.join("robot");
    let provider = robot.join("provider");
    fs::create_dir_all(provider.join("src"))?;
    fs::create_dir_all(robot.join("src"))?;
    fs::write(provider.join("Cargo.toml"), provider_manifest())?;
    fs::write(provider.join("src/main.rs"), PROVIDER_BIN)?;
    fs::write(robot.join("Cargo.toml"), robot_manifest())?;
    fs::write(robot.join("build.rs"), BUILD_RS)?;
    fs::write(robot.join("src/main.rs"), brain_main("u64", ""))?;
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-cycle-robot }\nservices:\n  provider:\n    source: { path: provider }\nconnections:\n  brain.telemetry: provider.provider_status\n",
    )?;

    let first = invoke(&robot, &["prepare", "--offline"]);
    assert!(
        first.status.success(),
        "the first preparation succeeds:\n{}",
        report(&first)
    );

    // Edit the brain's own message: the field's type changes, so the
    // brain's previously recorded copy of `Heartbeat` no longer matches
    // the authored definition. Re-preparation must refresh the record
    // instead of rejecting the conflict.
    fs::write(robot.join("src/main.rs"), brain_main("String", ""))?;
    let second = invoke(&robot, &["prepare", "--offline"]);
    assert!(
        second.status.success(),
        "editing the brain's own message keeps preparation green:\n{}",
        report(&second)
    );

    let check = invoke(&robot, &["check", "--offline"]);
    assert!(
        check.status.success(),
        "check validates the edited brain against the refreshed record:\n{}",
        report(&check)
    );

    // A further warm preparation reports no change: the refreshed record
    // is stable, not rewritten on every pass.
    let warm = invoke(&robot, &["prepare", "--offline"]);
    assert!(warm.status.success(), "{}", report(&warm));
    assert!(
        String::from_utf8_lossy(&warm.stdout).is_empty(),
        "the warm prepare reports no change:\n{}",
        report(&warm)
    );
    Ok(())
}

/// The brain authors a second message in the same package namespace the
/// participant provides. The recorded self products must skip only the
/// definitions the participant supplies — not the whole shared package —
/// so an unchanged repeated preparation keeps every type bound.
#[test]
fn shared_namespace_repeated_preparation_stays_green() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = root.join("robot");
    let provider = robot.join("provider");
    fs::create_dir_all(provider.join("src"))?;
    fs::create_dir_all(robot.join("src"))?;
    fs::write(provider.join("Cargo.toml"), provider_manifest())?;
    fs::write(provider.join("src/main.rs"), PROVIDER_BIN)?;
    fs::write(robot.join("Cargo.toml"), robot_manifest())?;
    fs::write(robot.join("build.rs"), BUILD_RS)?;
    // `Heartbeat` shares `proof.cycle.v1` with the provider's payload.
    fs::write(
        robot.join("src/main.rs"),
        brain_main("u64", "proof.cycle.v1"),
    )?;
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-cycle-robot }\nservices:\n  provider:\n    source: { path: provider }\nconnections:\n  brain.telemetry: provider.provider_status\n",
    )?;

    let first = invoke(&robot, &["prepare", "--offline"]);
    assert!(
        first.status.success(),
        "the first preparation succeeds:\n{}",
        report(&first)
    );

    // Nothing changed: the second preparation must keep the brain's own
    // `Heartbeat` beside the participant-provided `ProviderState`.
    let second = invoke(&robot, &["prepare", "--offline"]);
    assert!(
        second.status.success(),
        "an unchanged repeated preparation keeps every shared-namespace type:\n{}",
        report(&second)
    );
    let check = invoke(&robot, &["check", "--offline"]);
    assert!(
        check.status.success(),
        "check validates both definitions of the shared namespace:\n{}",
        report(&check)
    );
    Ok(())
}

fn provider_manifest() -> String {
    format!(
        "[package]\nname = \"proof-cycle-provider\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
         [dependencies]\n{}",
        phoxal_dep("\"runtime\"")
    )
}

/// The provider with one field added: the next revision of the same
/// package, selected by pinning the new commit.
const PROVIDER_BIN_REVISED: &str = r#"//! Provider revision B: the payload gains a field.
use phoxal::contracts::Latest;

#[phoxal::message(package = "proof.cycle.v1")]
pub struct ProviderState {
    #[phoxal(tag = 1)]
    pub value: u64,
    #[phoxal(tag = 2)]
    pub label: String,
}

#[phoxal::endpoints]
pub struct ProviderApi {
    #[phoxal::output(projection = state, max_bytes = 512)]
    provider_status: Latest<ProviderState>,
}

pub struct Provider {
    beats: u64,
}

#[phoxal::runtime(contract = ProviderApi, period_ms = 20)]
impl Provider {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self { beats: 0 })
    }

    #[step]
    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
        self.beats = self.beats.saturating_add(1);
        Ok(())
    }

    #[publish(provider_status)]
    fn provider_status(&self) -> ProviderState {
        ProviderState {
            value: self.beats,
            label: format!("step-{}", self.beats),
        }
    }
}

fn main() {}
"#;

fn git(source: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args([
            "-c",
            "user.name=Proof",
            "-c",
            "user.email=proof@example.test",
        ])
        .args(args)
        .current_dir(source)
        .output()
        .unwrap_or_else(|error| panic!("spawn git: {error}"))
}

#[test]
fn git_participant_revision_bump_keeps_preparation_green() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let source = root.join("source");
    let robot = root.join("robot");
    fs::create_dir_all(source.join("src"))?;
    fs::create_dir_all(robot.join("src"))?;
    fs::write(source.join("Cargo.toml"), provider_manifest())?;
    fs::write(source.join("src/main.rs"), PROVIDER_BIN)?;
    fs::write(robot.join("Cargo.toml"), robot_manifest())?;
    fs::write(robot.join("build.rs"), BUILD_RS)?;
    fs::write(robot.join("src/main.rs"), brain_main("u64", ""))?;

    // The provider carries real framework dependencies, so lockfile
    // resolution runs against the runner's cargo cache.
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--manifest-path"])
        .arg(source.join("Cargo.toml"))
        .current_dir(&source)
        .env(
            "CARGO_REGISTRIES_PHOXAL_INDEX",
            "sparse+https://phoxal.github.io/registry/",
        )
        .output()?;
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    assert!(git(&source, &["init"]).status.success());
    assert!(git(&source, &["add", "."]).status.success());
    assert!(
        git(&source, &["commit", "--quiet", "-m", "revision-a"])
            .status
            .success()
    );
    let revision = String::from_utf8(git(&source, &["rev-parse", "HEAD"]).stdout)?
        .trim()
        .to_owned();

    let robot_yaml = |revision: &str| {
        format!(
            "schema: phoxal/robot/v0\nrobot: {{ id: proof-cycle-robot }}\nservices:\n  provider:\n    source:\n      git:\n        name: proof-cycle-provider\n        url: file://{}\n        rev: {revision}\nconnections:\n  brain.telemetry: provider.provider_status\n",
            source.display()
        )
    };
    fs::write(robot.join("robot.yaml"), robot_yaml(&revision))?;

    // A local file:// Git source still requires a non-offline fetch,
    // exactly like the declaration-based Git flow.
    let first = invoke(&robot, &["prepare"]);
    assert!(
        first.status.success(),
        "the pinned Git revision prepares:\n{}",
        report(&first)
    );

    // Publish revision B of the same package — its payload gains a field —
    // and select it. The brain's recorded copy of the revision-A payload
    // must not veto the freshly prepared revision-B definition.
    fs::write(source.join("src/main.rs"), PROVIDER_BIN_REVISED)?;
    assert!(git(&source, &["add", "."]).status.success());
    assert!(
        git(&source, &["commit", "--quiet", "-m", "revision-b"])
            .status
            .success()
    );
    let next = String::from_utf8(git(&source, &["rev-parse", "HEAD"]).stdout)?
        .trim()
        .to_owned();
    fs::write(robot.join("robot.yaml"), robot_yaml(&next))?;

    let second = invoke(&robot, &["prepare"]);
    assert!(
        second.status.success(),
        "selecting the new revision prepares without hand-deleting caches:\n{}",
        report(&second)
    );
    let check = invoke(&robot, &["check", "--offline"]);
    assert!(
        check.status.success(),
        "check validates the brain against the revised participant:\n{}",
        report(&check)
    );
    Ok(())
}

/// A brain whose telemetry expectation is its own private type, reached
/// only through a robot-owned conversion from the participant's payload:
/// composition lowers the connection into the generated adapter. The
/// payload nests a message, an enumeration, and a payload enum so the
/// adapter's retained schema must cover the complete reachable closure.
fn converting_brain_main() -> String {
    r#"//! Brain: expects its own private telemetry record; the robot converts
//! the provider's payload into it inside this same executable, which also
//! hosts the generated conversion role. The authored conversion module
//! reads generated bindings, so it compiles only once the package's own
//! prepared products exist (the same cfg integration tests gate on).
#[cfg(phoxal_self_prepared)]
mod conversions;

phoxal::api!();
phoxal::conversions!();

use phoxal::contracts::Latest;

#[phoxal::message]
pub struct TelemetryDetail {
    #[phoxal(tag = 1)]
    pub label: String,
}

#[phoxal::message]
pub enum TelemetryNote {
    #[phoxal(tag = 1)]
    Plain(String),
    #[phoxal(tag = 2)]
    Sealed(TelemetryDetail),
}

#[phoxal::message]
pub enum TelemetryGrade {
    Unspecified = 0,
    Trusted = 1,
}

#[phoxal::message]
pub struct TelemetryIn {
    #[phoxal(tag = 1)]
    pub beats: u64,
    #[phoxal(tag = 2)]
    pub detail: Option<TelemetryDetail>,
    #[phoxal(tag = 3)]
    pub note: Option<TelemetryNote>,
    #[phoxal(tag = 4)]
    pub grade: TelemetryGrade,
}

#[phoxal::endpoints(package = "proof.cycle.brain.v1")]
pub struct BrainApi {
    #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
    telemetry: Latest<TelemetryIn>,
}

pub struct Brain {
    beats: u64,
}

#[phoxal::runtime(contract = BrainApi, period_ms = 50)]
impl Brain {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self { beats: 0 })
    }

    #[step]
    fn advance(&mut self, ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
        let _ = ctx.telemetry().value();
        self.beats = self.beats.saturating_add(1);
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    run_hosted_roles(phoxal_runtime_brain::Adapter::new())
}
"#
    .to_owned()
}

/// The robot-owned conversion the generated adapter compiles against.
/// The nested detail, payload-enum note, and grade fields stay absent or
/// neutral: the conversion only needs the reachable schema to exist.
const CONVERSIONS: &str = r#"//! Robot-owned conversions for composed telemetry expectations.

use crate::api::brain::TelemetryIn;
use crate::api::provider::ProviderState;

impl From<ProviderState> for TelemetryIn {
    fn from(source: ProviderState) -> TelemetryIn {
        TelemetryIn {
            beats: source.value,
            ..Default::default()
        }
    }
}
"#;

/// The hosted conversion role's compiled contract stays consumable: no
/// generated source-tree file or Cargo target ever appears, the discovered
/// edges persist as the sidecar plan, and bundle assembly validates the
/// hosted record's retained schema — including the brain-side payload's
/// nested messages, enumerations, and payload-enum variants the conversion
/// serves, whose generated bindings carry no frames in the brain's own
/// compilation.
#[test]
fn adapter_compiled_contract_stays_consumable() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = root.join("robot");
    let provider = robot.join("provider");
    fs::create_dir_all(provider.join("src"))?;
    fs::create_dir_all(robot.join("src"))?;
    fs::write(provider.join("Cargo.toml"), provider_manifest())?;
    fs::write(provider.join("src/main.rs"), PROVIDER_BIN)?;
    fs::write(robot.join("Cargo.toml"), robot_manifest())?;
    fs::write(robot.join("build.rs"), BUILD_RS)?;
    fs::write(robot.join("src/main.rs"), converting_brain_main())?;
    fs::write(robot.join("src/conversions.rs"), CONVERSIONS)?;
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-cycle-robot }\nbrain: { binary: proof-cycle-robot }\nservices:\n  provider:\n    source: { path: provider }\nconnections:\n  brain.telemetry: provider.provider_status\n",
    )?;

    let first = invoke(&robot, &["prepare", "--offline"]);
    assert!(
        first.status.success(),
        "the converting composition prepares:\n{}",
        report(&first)
    );
    let check = invoke(&robot, &["check", "--offline"]);
    assert!(
        check.status.success(),
        "check compiles the hosted conversion role and its conversion:\n{}",
        report(&check)
    );
    assert!(
        !robot.join("src/bin/phoxal-adapter.rs").exists(),
        "no generated adapter target may appear in the authored tree"
    );
    let plan = robot.join(".phoxal/conversions/edges.json");
    assert!(
        plan.is_file(),
        "the discovered edges persist as the sidecar"
    );
    let plan_text = fs::read_to_string(&plan)?;
    assert!(
        plan_text.contains("brain.telemetry") && plan_text.contains("provider.provider_status"),
        "the sidecar records the conversion edge: {plan_text}"
    );

    // Consume the hosted record's compiled contract: bundle assembly
    // extracts the named hosted record from the robot executable and
    // validates its complete retained closure through the descriptor
    // consistency check.
    let build = invoke(&robot, &["build", "--offline"]);
    assert!(
        build.status.success(),
        "the bundle consumes the hosted conversion record:\n{}",
        report(&build)
    );
    Ok(())
}
