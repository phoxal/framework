use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

mod installation;
mod project;

// Re-export the public project surface so that paths like
// `crate::ProjectLayout` continue to work for the module's own internal
// call sites the same way they did when phoxal-project was an external
// crate. The project module owns its `pub use` list; this mirrors it
// at the cargo-phoxal root.
pub use project::*;

fn main() -> ExitCode {
    let cli = Cli::parse_from(cargo_arguments(std::env::args_os()));
    let json_diagnostics = cli.json_diagnostics();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            print_error(&error, json_diagnostics);
            ExitCode::FAILURE
        }
    }
}

// Cargo external subcommands receive their own name as argv[1].
fn cargo_arguments(arguments: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut arguments: Vec<_> = arguments.into_iter().collect();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "phoxal")
    {
        arguments.remove(1);
    }
    arguments
}

fn run(cli: Cli) -> Result<(), crate::project::Error> {
    let command = cli.command;
    match command {
        Command::Publish(arguments) => run_publication(arguments),
        Command::Simulation(arguments) => match arguments.command {
            SimulationCommand::Run(arguments) => run_simulation(arguments),
            SimulationCommand::Scenario(arguments) => match arguments.command {
                ScenarioCommand::List(arguments) => run_scenario_list(arguments),
                ScenarioCommand::Run(arguments) => run_scenario_case(arguments),
            },
        },
        command => {
            let project = Project::discover(std::env::current_dir().map_err(|source| {
                crate::project::Error::Discovery(crate::project::DiscoveryError::Resolve {
                    path: ".".into(),
                    source,
                })
            })?)?;
            match command {
                Command::Check(arguments) => run_cargo(
                    &project,
                    CargoOperation::Check,
                    arguments.into_options(Vec::new()),
                ),
                Command::Build(arguments) => {
                    let output = arguments.output.clone();
                    let options = arguments.into_options();
                    let prepared = project.prepare(&options)?;
                    print_preparation(&prepared);
                    let output = output.unwrap_or_else(|| prepared.default_bundle_path());
                    let bundle = prepared.build_bundle(&options, output)?;
                    print_status(
                        &options,
                        &format!("compiled bundle: {}", bundle.root().display()),
                    );
                    Ok(())
                }
                Command::Run(arguments) => {
                    let output = arguments.output.clone();
                    let options = arguments.into_options();
                    let prepared = project.prepare(&options)?;
                    print_preparation(&prepared);
                    let output = output.unwrap_or_else(|| prepared.default_bundle_path());
                    let bundle = prepared.run_local(&options, output)?;
                    print_status(
                        &options,
                        &format!("compiled bundle: {}", bundle.root().display()),
                    );
                    Ok(())
                }
                Command::Test(arguments) => run_cargo(
                    &project,
                    CargoOperation::Test,
                    arguments
                        .options
                        .into_options(Vec::new(), arguments.test_args),
                ),
                Command::Update(arguments) => {
                    let options = arguments.into_options();
                    let outputs = project.update(&options)?;
                    for output in outputs {
                        print_bytes(&output.stdout, false);
                        print_bytes(&output.stderr, true);
                    }
                    Ok(())
                }
                Command::Simulation(_) => unreachable!("simulation was handled above"),
                Command::Publish(_) => unreachable!("publish was handled above"),
            }
        }
    }
}

fn run_simulation(arguments: SimulationRunArgs) -> Result<(), crate::project::Error> {
    let SimulationRunArgs {
        scene,
        headless,
        desktop: _,
        steps,
        duration,
        simulator,
        output,
        scope,
        supervisor_id,
        run_id,
        options,
    } = arguments;
    let presentation = if headless {
        SimulationPresentation::Headless
    } else {
        SimulationPresentation::Desktop
    };
    let bound = match (steps, duration) {
        (Some(steps), None) => SimulationBound::Steps(steps),
        (None, Some(duration)) => SimulationBound::Duration(duration),
        (None, None) => SimulationBound::Steps(1),
        (Some(_), Some(_)) => {
            return Err(crate::project::Error::SimulationInvalid {
                message: "choose either --steps or --duration".to_owned(),
            });
        }
    };
    let mut request = SimulationRunOptions::new(scene, presentation, bound)?;
    if let Some(path) = simulator {
        request = request.with_simulator_executable(path);
    }
    if let Some(path) = output {
        request = request.with_output(path);
    }
    if scope.is_some() || supervisor_id.is_some() || run_id.is_some() {
        request = request.with_identity(
            scope.unwrap_or_else(|| "local".to_owned()),
            supervisor_id.unwrap_or_else(|| "local".to_owned()),
            run_id.unwrap_or_else(|| "local-simulation".to_owned()),
        );
    }
    let cargo_options = options.into_options(Vec::new(), Vec::new());
    let project = Project::discover(std::env::current_dir().map_err(|source| {
        crate::project::Error::Discovery(crate::project::DiscoveryError::Resolve {
            path: ".".into(),
            source,
        })
    })?)?;
    let report = project.run_simulation(&cargo_options, &request)?;
    if !report.simulator_stdout.trim().is_empty() {
        print!("{}", report.simulator_stdout);
        if !report.simulator_stdout.ends_with('\n') {
            println!();
        }
    }
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|source| {
            crate::project::Error::SimulationInvalid {
                message: format!("cannot encode simulation terminal report: {source}"),
            }
        })?
    );
    if !report.simulator_stderr.is_empty() {
        eprint!("{}", report.simulator_stderr);
    }
    eprintln!(
        "simulation: simulator={} bundle={} provider_contract={} cleanup={}",
        report.simulator.executable.display(),
        report.bundle.display(),
        if report.provider_contract_verified {
            "verified"
        } else {
            "unverified"
        },
        if report.cleanup.error.is_none() {
            "complete"
        } else {
            "incomplete"
        }
    );
    if report.success() {
        Ok(())
    } else {
        Err(crate::project::Error::SimulationInvalid {
            message: format!(
                "simulation did not complete successfully (exit={}, supervisor_ready={}, provider_contract_verified={}, cleanup={})",
                report
                    .simulator_exit_code
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                report.supervisor_ready,
                report.provider_contract_verified,
                if report.cleanup.error.is_none() {
                    "complete"
                } else {
                    "incomplete"
                }
            ),
        })
    }
}

fn run_scenario_list(arguments: ScenarioListArgs) -> Result<(), crate::project::Error> {
    let ScenarioListArgs { filter, options } = arguments;
    let project = Project::discover(std::env::current_dir().map_err(|source| {
        crate::project::Error::Discovery(crate::project::DiscoveryError::Resolve {
            path: ".".into(),
            source,
        })
    })?)?;
    let cargo_options = options.into_options(Vec::new(), Vec::new());
    let entries = crate::project::scenario::list_scenarios(&project, &cargo_options)?;
    for entry in entries {
        if filter
            .as_deref()
            .is_none_or(|needle| entry.name.contains(needle))
        {
            println!("{}", entry.name);
        }
    }
    Ok(())
}

fn run_scenario_case(arguments: ScenarioRunArgs) -> Result<(), crate::project::Error> {
    let ScenarioRunArgs { scenario, options } = arguments;
    let project = Project::discover(std::env::current_dir().map_err(|source| {
        crate::project::Error::Discovery(crate::project::DiscoveryError::Resolve {
            path: ".".into(),
            source,
        })
    })?)?;
    let cargo_options = options.into_options(Vec::new(), Vec::new());
    let outcome = crate::project::scenario::run_scenario(&project, &cargo_options, &scenario)?;
    println!(
        "scenario {}: {}",
        outcome.scenario_name,
        if outcome.passed { "PASSED" } else { "FAILED" }
    );
    if !outcome.stdout.is_empty() {
        for line in outcome.stdout.lines() {
            println!("  {line}");
        }
    }
    if !outcome.stderr.is_empty() {
        for line in outcome.stderr.lines() {
            eprintln!("  {line}");
        }
    }
    if let Some(path) = outcome.report_artifact_path {
        println!("  report: {}", path.display());
    }
    if !outcome.passed {
        std::process::exit(1);
    }
    Ok(())
}

fn run_publication(arguments: PublishArgs) -> Result<(), crate::project::Error> {
    let (kind, package) = match arguments.package {
        PublishPackage::Component(package) => (PublicationKind::Component, package),
        PublishPackage::Service(package) => (PublicationKind::Service, package),
    };
    let dry_run = package.dry_run;
    let result = prepare_publication(&PublicationOptions {
        kind,
        name: package.name,
        path: package.path,
        dry_run,
    })?;
    println!(
        "publication: {}",
        if dry_run { "dry-run" } else { "prepared" }
    );
    println!("kind: {}", result.kind());
    println!("package: {}", result.package());
    println!("version: {}", result.version());
    println!("archive: {}", result.archive().display());
    println!("sha256: {}", result.checksum());
    println!("inventory: {}", result.inventory().display());
    println!("checksum-file: {}", result.checksum_file().display());
    println!("bytes: {}", result.bytes());
    println!("files:");
    for file in result.files() {
        println!("  {} {} {}", file.path, file.bytes, file.sha256);
    }
    if !dry_run {
        let submission = submit_publication(&result, |authorization| {
            eprintln!(
                "Authorize cargo-phoxal at {} with code {} (expires in {} seconds).",
                authorization.verification_uri,
                authorization.user_code,
                authorization.expires_in.as_secs()
            );
            eprintln!(
                "GitHub's public_repo scope covers every public repository accessible to this account."
            );
        })?;
        match submission {
            SubmissionResult::Available { archive_url } => {
                println!("publication: available");
                println!("archive-url: {archive_url}");
            }
            SubmissionResult::PendingReview {
                pull_request_url,
                branch,
            } => {
                println!("publication: pending-review");
                println!("branch: {branch}");
                println!("pull-request: {pull_request_url}");
            }
        }
    }
    Ok(())
}

fn run_cargo(
    project: &Project,
    operation: CargoOperation,
    options: CargoOptions,
) -> Result<(), crate::project::Error> {
    let json = json_requested(&options);
    let prepared = project.prepare(&options)?;
    print_preparation(&prepared);
    let outputs = if operation == CargoOperation::Check {
        prepared.check(&options)?
    } else {
        prepared.run(operation, &options)?
    };
    for output in outputs {
        print_bytes(&output.stdout, false);
        print_bytes(&output.stderr, true);
    }
    if json {
        eprintln!("cargo phoxal: {} completed", operation.as_str());
    }
    Ok(())
}

fn print_status(options: &CargoOptions, message: &str) {
    if json_requested(options) {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

fn json_requested(options: &CargoOptions) -> bool {
    json_format(options.message_format.as_deref(), &options.cargo_args)
}

fn json_common(options: &CommonArgs, cargo_args: &[OsString]) -> bool {
    json_format(options.message_format.as_deref(), cargo_args)
}

fn json_format(explicit: Option<&str>, arguments: &[OsString]) -> bool {
    if explicit.is_some_and(|format| format.starts_with("json")) {
        return true;
    }
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.to_string_lossy();
        if let Some(format) = argument.strip_prefix("--message-format=") {
            return format.starts_with("json");
        }
        if argument == "--message-format"
            && arguments
                .next()
                .is_some_and(|format| format.to_string_lossy().starts_with("json"))
        {
            return true;
        }
    }
    false
}

fn print_error(error: &crate::project::Error, json: bool) {
    if !json {
        eprintln!("error: {error:#}");
        return;
    }
    let span = diagnostic_path(error).map(|path| {
        serde_json::json!({
            "file_name": path,
            "byte_start": 0,
            "byte_end": 0,
            "line_start": 1,
            "line_end": 1,
            "column_start": 1,
            "column_end": 1,
            "is_primary": true,
            "label": "Phoxal project diagnostic"
        })
    });
    let diagnostic = serde_json::json!({
        "reason": "phoxal-diagnostic",
        "message": format!("{error:#}"),
        "level": "error",
        "spans": span.into_iter().collect::<Vec<_>>(),
        "children": [],
        "rendered": format!("error: {error:#}\n")
    });
    eprintln!("{diagnostic}");
}

fn diagnostic_path(error: &crate::project::Error) -> Option<PathBuf> {
    match error {
        crate::project::Error::Discovery(error) => match error {
            crate::project::DiscoveryError::Resolve { path, .. }
            | crate::project::DiscoveryError::MissingRobot { start: path }
            | crate::project::DiscoveryError::MissingManifest { root: path } => Some(path.clone()),
        },
        crate::project::Error::ReadRobot { path, .. }
        | crate::project::Error::ParseRobot { path, .. }
        | crate::project::Error::InvalidRobot { path, .. }
        | crate::project::Error::ReadManifest { path, .. }
        | crate::project::Error::ParseManifest { path, .. }
        | crate::project::Error::VirtualManifest { path }
        | crate::project::Error::MissingInitialization { path, .. }
        | crate::project::Error::ManifestPreparation { path, .. }
        | crate::project::Error::ManifestWrite { path, .. }
        | crate::project::Error::CargoLockWrite { path, .. }
        | crate::project::Error::CargoLockRead { path, .. }
        | crate::project::Error::ManifestRestore { path, .. }
        | crate::project::Error::CargoMetadata { manifest: path, .. } => Some(path.clone()),
        crate::project::Error::ConfigurationInvalid { .. }
        | crate::project::Error::Source(_)
        | crate::project::Error::CargoCommand { .. }
        | crate::project::Error::CargoSpawn { .. }
        | crate::project::Error::InvalidOptions { .. }
        | crate::project::Error::ArtifactCapture { .. }
        | crate::project::Error::MissingArtifactContract { .. }
        | crate::project::Error::SimulationInvalid { .. }
        | crate::project::Error::BundleSourceChanged { .. }
        | crate::project::Error::SupervisorLaunch { .. }
        | crate::project::Error::ArtifactInvalid { .. }
        | crate::project::Error::ArtifactFile { .. }
        | crate::project::Error::BundleDirectory { .. }
        | crate::project::Error::BundleCopy { .. }
        | crate::project::Error::BundleJson { .. }
        | crate::project::Error::BundleWrite { .. }
        | crate::project::Error::BundleBusy { .. }
        | crate::project::Error::BundleLock { .. }
        | crate::project::Error::BundlePublish { .. }
        | crate::project::Error::BundleCleanup { .. }
        | crate::project::Error::InvalidExecutionIdentity { .. }
        | crate::project::Error::Publication(_) => None,
        crate::project::Error::ScenarioRun(_) => None,
    }
}

fn print_preparation(prepared: &crate::project::PreparedProject) {
    for change in prepared.preparation_changes() {
        match change {
            crate::project::PreparationChange::SupervisorDependencyAdded {
                dependency,
                requirement,
            } => eprintln!("prepared dependency {dependency} ({requirement})"),
            crate::project::PreparationChange::TestTargetAdded { name, path } => {
                eprintln!("prepared test target `{name}` at {path}")
            }
            crate::project::PreparationChange::DevDependencyFeatureAdded {
                dependency,
                feature,
            } => eprintln!("prepared dev-dep `{dependency}` feature `{feature}`"),
            crate::project::PreparationChange::HarnessWritten { path } => {
                eprintln!("prepared harness at {path}")
            }
        }
    }
}

fn print_bytes(bytes: &[u8], stderr: bool) {
    if bytes.is_empty() {
        return;
    }
    if stderr {
        eprint!("{}", String::from_utf8_lossy(bytes));
    } else {
        print!("{}", String::from_utf8_lossy(bytes));
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "cargo phoxal",
    bin_name = "cargo phoxal",
    about = "Validate and build a Phoxal robot project"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn json_diagnostics(&self) -> bool {
        match &self.command {
            Command::Check(arguments) => json_common(&arguments.options, &arguments.cargo_args),
            Command::Build(arguments) | Command::Run(arguments) => {
                json_common(&arguments.options, &arguments.cargo_args)
            }
            Command::Test(arguments) => json_common(&arguments.options, &[]),
            Command::Update(arguments) => json_common(&arguments.options, &arguments.cargo_args),
            Command::Simulation(arguments) => match &arguments.command {
                SimulationCommand::Run(arguments) => json_common(&arguments.options, &[]),
                SimulationCommand::Scenario(arguments) => match &arguments.command {
                    ScenarioCommand::List(arguments) => json_common(&arguments.options, &[]),
                    ScenarioCommand::Run(arguments) => json_common(&arguments.options, &[]),
                },
            },
            Command::Publish(_) => false,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prepare the project, validate composition, and Cargo-check selected targets.
    Check(CommandArgs),
    /// Prepare the project, validate composition, and build selected targets.
    Build(BuildArgs),
    /// Prepare, build, validate, and launch the selected supervisor locally.
    Run(BuildArgs),
    /// Prepare the project and run tests for the root robot package.
    Test(TestArgs),
    /// Resolve permitted Cargo updates and validate the resulting Phoxal graph.
    Update(UpdateArgs),
    /// Provision and run the independent native simulator application.
    Simulation(SimulationArgs),
    /// Prepare an authored component or service package for registry review.
    Publish(PublishArgs),
}

#[derive(Debug, Args)]
struct SimulationArgs {
    #[command(subcommand)]
    command: SimulationCommand,
}

#[derive(Debug, Subcommand)]
enum SimulationCommand {
    /// Run one finite scene against the selected robot bundle.
    Run(SimulationRunArgs),
    /// List, plan, or run authored scenarios for the selected robot bundle.
    Scenario(ScenarioArgs),
}

#[derive(Debug, Args)]
struct ScenarioArgs {
    #[command(subcommand)]
    command: ScenarioCommand,
}

#[derive(Debug, Subcommand)]
enum ScenarioCommand {
    /// List scenarios registered in the prepared harness binary.
    List(ScenarioListArgs),
    /// Run a single scenario by struct identity (e.g. `RoverForwardTurnStop`).
    Run(ScenarioRunArgs),
}

#[derive(Debug, Args)]
struct ScenarioListArgs {
    /// Optional substring filter against the registered scenario names.
    filter: Option<String>,
    #[command(flatten)]
    options: CommonArgs,
}

#[derive(Debug, Args)]
struct ScenarioRunArgs {
    /// Scenario struct identity (`<StructIdent>`, not the full `scenarios/...` prefix).
    scenario: String,
    #[command(flatten)]
    options: CommonArgs,
}

#[derive(Debug, Args)]
struct SimulationRunArgs {
    /// Scene MJCF or MJZ archive.
    scene: PathBuf,
    /// Run without opening a presentation window.
    #[arg(long, conflicts_with = "desktop")]
    headless: bool,
    /// Run with the simulator desktop presentation.
    #[arg(long, conflicts_with = "headless")]
    desktop: bool,
    /// Advance exactly this many native quanta.
    #[arg(long, conflicts_with = "duration")]
    steps: Option<u64>,
    /// Advance exactly this many seconds, requiring an integral quantum count.
    #[arg(long, conflicts_with = "steps")]
    duration: Option<f64>,
    /// Explicit simulator executable injection or installed artifact path.
    #[arg(long)]
    simulator: Option<PathBuf>,
    /// Compiled simulation bundle output path.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Router namespace for this local launch.
    #[arg(long)]
    scope: Option<String>,
    /// Supervisor identity within the router namespace.
    #[arg(long)]
    supervisor_id: Option<String>,
    /// Finite run identity passed to the simulator.
    #[arg(long)]
    run_id: Option<String>,
    #[command(flatten)]
    options: CommonArgs,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    #[command(flatten)]
    options: CommonArgs,
    /// Additional arguments passed to `cargo update`.
    #[arg(last = true, allow_hyphen_values = true)]
    cargo_args: Vec<OsString>,
}

impl UpdateArgs {
    fn into_options(self) -> CargoOptions {
        self.options.into_options(self.cargo_args, Vec::new())
    }
}

#[derive(Debug, Args)]
struct PublishArgs {
    #[command(subcommand)]
    package: PublishPackage,
}

#[derive(Debug, Subcommand)]
enum PublishPackage {
    /// Prepare a component package, including a targetless passive carrier.
    Component(PublishPackageArgs),
    /// Prepare a service implementation or configuration preset package.
    Service(PublishPackageArgs),
}

#[derive(Debug, Args)]
struct PublishPackageArgs {
    /// Exact Cargo package name from the authored Cargo.toml.
    name: String,
    /// Source directory containing the authored Cargo.toml.
    #[arg(long)]
    path: Option<PathBuf>,
    /// Prepare and verify locally without GitHub authentication or mutation.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct CommandArgs {
    #[command(flatten)]
    options: CommonArgs,
    /// Additional arguments passed to Cargo after Phoxal's standard selectors.
    #[arg(last = true, allow_hyphen_values = true)]
    cargo_args: Vec<OsString>,
}

impl CommandArgs {
    fn into_options(self, trailing: Vec<OsString>) -> CargoOptions {
        self.options.into_options(self.cargo_args, trailing)
    }
}

#[derive(Debug, Args)]
struct BuildArgs {
    #[command(flatten)]
    options: CommonArgs,
    /// Compiled bundle directory, defaulting below Cargo's target directory.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Additional arguments passed to Cargo after Phoxal's standard selectors.
    #[arg(last = true, allow_hyphen_values = true)]
    cargo_args: Vec<OsString>,
}

impl BuildArgs {
    fn into_options(self) -> CargoOptions {
        self.options.into_options(self.cargo_args, Vec::new())
    }
}

#[derive(Debug, Args)]
struct TestArgs {
    #[command(flatten)]
    options: CommonArgs,
    /// Arguments passed to the root test binary after Cargo's test delimiter.
    #[arg(last = true, allow_hyphen_values = true)]
    test_args: Vec<OsString>,
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Cargo executable to use for every metadata, build, check, test, and update invocation.
    #[arg(long, env = "CARGO", hide_env_values = true)]
    cargo: Option<PathBuf>,
    /// Require an existing current Cargo.lock.
    #[arg(long, conflicts_with = "frozen")]
    locked: bool,
    /// Require an existing current Cargo.lock and forbid network access.
    #[arg(long)]
    frozen: bool,
    /// Forbid network access while allowing Cargo's normal lock policy.
    #[arg(long)]
    offline: bool,
    /// Cargo target triple.
    #[arg(long)]
    target: Option<String>,
    /// Cargo profile.
    #[arg(long)]
    profile: Option<String>,
    /// Comma-separated or repeated root feature names.
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,
    /// Enable all root features.
    #[arg(long, conflicts_with = "no_default_features")]
    all_features: bool,
    /// Disable default root features.
    #[arg(long = "no-default-features")]
    no_default_features: bool,
    /// Build with Cargo's release profile.
    #[arg(long)]
    release: bool,
    /// Cargo compiler message format, such as `json`.
    #[arg(long)]
    message_format: Option<String>,
    /// Select the whole Cargo workspace.
    #[arg(long)]
    workspace: bool,
    /// Select an explicit Cargo package, repeatable.
    #[arg(long = "package", short = 'p', action = clap::ArgAction::Append)]
    packages: Vec<String>,
    /// Exclude a workspace package, repeatable.
    #[arg(long = "exclude", action = clap::ArgAction::Append)]
    excludes: Vec<String>,
    /// Select all targets.
    #[arg(long)]
    all_targets: bool,
    /// Select the package library target.
    #[arg(long)]
    lib: bool,
    /// Select all binary targets.
    #[arg(long)]
    bins: bool,
    /// Select a named binary target, repeatable.
    #[arg(long = "bin", action = clap::ArgAction::Append)]
    binaries: Vec<String>,
    /// Select all example targets.
    #[arg(long)]
    examples: bool,
    /// Select a named example target, repeatable.
    #[arg(long = "example", action = clap::ArgAction::Append)]
    examples_named: Vec<String>,
    /// Select all integration tests.
    #[arg(long)]
    tests: bool,
    /// Select a named integration test, repeatable.
    #[arg(long = "test", action = clap::ArgAction::Append)]
    tests_named: Vec<String>,
    /// Select all benchmarks.
    #[arg(long)]
    benches: bool,
    /// Select a named benchmark, repeatable.
    #[arg(long = "bench", action = clap::ArgAction::Append)]
    benches_named: Vec<String>,
}

impl CommonArgs {
    fn into_options(self, cargo_args: Vec<OsString>, test_args: Vec<OsString>) -> CargoOptions {
        CargoOptions {
            cargo_path: self.cargo,
            lock: if self.frozen {
                LockMode::Frozen
            } else if self.locked {
                LockMode::Locked
            } else {
                LockMode::Unlocked
            },
            offline: self.offline,
            target: self.target,
            profile: self.profile,
            features: self.features,
            all_features: self.all_features,
            no_default_features: self.no_default_features,
            release: self.release,
            message_format: self.message_format,
            cargo_args,
            test_args,
            selection: CargoSelection {
                workspace: self.workspace,
                packages: self.packages,
                excludes: self.excludes,
                all_targets: self.all_targets,
                lib: self.lib,
                bins: self.bins,
                binaries: self.binaries,
                examples: self.examples,
                examples_named: self.examples_named,
                tests: self.tests,
                tests_named: self.tests_named,
                benches: self.benches,
                benches_named: self.benches_named,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cargo_external_subcommand_and_direct_invocation_parse_identically() {
        for arguments in [
            vec!["cargo-phoxal", "phoxal", "check", "--locked"],
            vec!["cargo-phoxal", "check", "--locked"],
        ] {
            let parsed = Cli::try_parse_from(super::cargo_arguments(
                arguments.into_iter().map(OsString::from),
            ))
            .unwrap();
            assert!(matches!(parsed.command, Command::Check(_)));
        }
    }

    #[test]
    fn documented_commands_parse_and_preserve_lock_policy() {
        let parsed = Cli::try_parse_from(["cargo-phoxal", "check", "--locked"])
            .expect("check command parses");
        let options = match parsed.command {
            Command::Check(arguments) => arguments.into_options(Vec::new()),
            _ => panic!("the check command parsed as a different variant"),
        };
        assert_eq!(options.lock, LockMode::Locked);

        let parsed =
            Cli::try_parse_from(["cargo-phoxal", "test", "--frozen"]).expect("test command parses");
        let options = match parsed.command {
            Command::Test(arguments) => arguments
                .options
                .into_options(Vec::new(), arguments.test_args),
            _ => panic!("the test command parsed as a different variant"),
        };
        assert_eq!(options.lock, LockMode::Frozen);
    }

    #[test]
    fn test_arguments_after_the_delimiter_are_not_sent_to_metadata() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "test",
            "--offline",
            "--",
            "--exact",
            "brain_tests::starts_empty",
        ])
        .expect("test arguments parse");
        let options = match parsed.command {
            Command::Test(arguments) => arguments
                .options
                .into_options(Vec::new(), arguments.test_args),
            _ => panic!("the test command parsed as a different variant"),
        };
        assert!(options.cargo_args.is_empty());
        assert_eq!(
            options.test_args,
            [
                OsString::from("--exact"),
                OsString::from("brain_tests::starts_empty")
            ]
        );
    }

    #[test]
    fn test_delimiter_does_not_turn_test_binary_arguments_into_json_diagnostics() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "test",
            "--",
            "--message-format=json-render-diagnostics",
        ])
        .expect("test binary arguments parse");
        assert!(!parsed.json_diagnostics());
    }

    #[test]
    fn build_boundary_parses_without_extra_process_options() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "build",
            "--output",
            "target/bundle",
            "--offline",
        ])
        .expect("build command parses");
        assert!(matches!(parsed.command, Command::Build(_)));
    }

    #[test]
    fn run_boundary_preserves_the_build_output_and_cargo_argument_surface() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "run",
            "--output",
            "target/run-bundle",
            "--target",
            "aarch64-unknown-linux-gnu",
            "--",
            "--release",
        ])
        .expect("run command parses");
        let arguments = match parsed.command {
            Command::Run(arguments) => arguments,
            _ => panic!("the run command parsed as a different variant"),
        };
        assert_eq!(arguments.output, Some(PathBuf::from("target/run-bundle")));
        let options = arguments.into_options();
        assert_eq!(options.target.as_deref(), Some("aarch64-unknown-linux-gnu"));
        assert!(!options.release);
        assert_eq!(options.cargo_args, [OsString::from("--release")]);
    }

    #[test]
    fn simulation_run_parses_scene_mode_bound_and_lock_policy() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "simulation",
            "run",
            "scene.xml",
            "--headless",
            "--steps",
            "4",
            "--locked",
            "--scope",
            "workshop",
            "--supervisor-id",
            "rover-01",
            "--run-id",
            "run-1",
        ])
        .expect("simulation run parses");
        let arguments = match parsed.command {
            Command::Simulation(arguments) => match arguments.command {
                SimulationCommand::Run(arguments) => arguments,
                SimulationCommand::Scenario(_) => {
                    panic!("simulation command parsed as a scenario")
                }
            },
            _ => panic!("simulation command parsed as a different variant"),
        };
        assert_eq!(arguments.scene, PathBuf::from("scene.xml"));
        assert!(arguments.headless);
        assert!(!arguments.desktop);
        assert_eq!(arguments.steps, Some(4));
        assert_eq!(arguments.duration, None);
        assert_eq!(arguments.scope.as_deref(), Some("workshop"));
        assert_eq!(arguments.supervisor_id.as_deref(), Some("rover-01"));
        assert_eq!(arguments.run_id.as_deref(), Some("run-1"));
        assert_eq!(
            arguments.options.into_options(Vec::new(), Vec::new()).lock,
            LockMode::Locked
        );
    }

    #[test]
    fn simulation_run_rejects_conflicting_modes_and_bounds() {
        assert!(
            Cli::try_parse_from([
                "cargo-phoxal",
                "simulation",
                "run",
                "scene.xml",
                "--headless",
                "--desktop",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "cargo-phoxal",
                "simulation",
                "run",
                "scene.xml",
                "--steps",
                "1",
                "--duration",
                "0.01",
            ])
            .is_err()
        );
    }

    #[test]
    fn publication_supports_normal_submission_and_explicit_dry_run() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "publish",
            "component",
            "example-passive-caster",
            "--dry-run",
        ])
        .expect("publication dry-run parses");
        assert!(matches!(parsed.command, Command::Publish(_)));
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "publish",
            "component",
            "example-passive-caster",
        ])
        .expect("normal publication parses");
        assert!(matches!(parsed.command, Command::Publish(_)));
    }

    #[test]
    fn cargo_path_and_native_package_selection_are_preserved() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "test",
            "--cargo",
            "/opt/cargo/bin/cargo",
            "--workspace",
            "--package",
            "robot",
            "--exclude",
            "fixture",
            "--all-targets",
            "--release",
            "--message-format",
            "json-render-diagnostics",
            "--",
            "--nocapture",
        ])
        .expect("Cargo selectors parse");
        let options = match parsed.command {
            Command::Test(arguments) => arguments
                .options
                .into_options(Vec::new(), arguments.test_args),
            _ => panic!("the test command parsed as a different variant"),
        };
        assert_eq!(
            options.cargo_path,
            Some(PathBuf::from("/opt/cargo/bin/cargo"))
        );
        assert!(options.selection.workspace);
        assert_eq!(options.selection.packages, ["robot"]);
        assert_eq!(options.selection.excludes, ["fixture"]);
        assert!(options.selection.all_targets);
        assert!(options.release);
        assert!(options.cargo_args.is_empty());
        assert_eq!(options.test_args, [OsString::from("--nocapture")]);
        assert!(json_requested(&options));
    }

    #[test]
    fn update_command_preserves_its_cargo_boundary() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "update",
            "--offline",
            "--package",
            "robot",
            "--",
            "--dry-run",
        ])
        .expect("update command parses");
        let options = match parsed.command {
            Command::Update(arguments) => arguments.into_options(),
            _ => panic!("the update command parsed as a different variant"),
        };
        assert!(options.offline);
        assert_eq!(options.selection.packages, ["robot"]);
        assert_eq!(options.cargo_args, [OsString::from("--dry-run")]);
    }
}
