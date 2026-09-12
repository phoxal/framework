use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use phoxal_project::{
    CargoOperation, CargoOptions, LockMode, Project, PublicationKind, PublicationOptions,
    prepare_publication,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), phoxal_project::Error> {
    let command = Cli::parse().command;
    match command {
        Command::Publish(arguments) => run_publication(arguments),
        command => {
            let project = Project::discover(std::env::current_dir().map_err(|source| {
                phoxal_project::Error::Discovery(phoxal_project::DiscoveryError::Resolve {
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
                    let output = output.unwrap_or_else(|| prepared.default_bundle_path());
                    let bundle = prepared.build_bundle(&options, output)?;
                    println!("compiled bundle: {}", bundle.root().display());
                    Ok(())
                }
                Command::Test(arguments) => run_cargo(
                    &project,
                    CargoOperation::Test,
                    arguments
                        .options
                        .into_options(Vec::new(), arguments.test_args),
                ),
                Command::Publish(_) => unreachable!("publish was handled above"),
            }
        }
    }
}

fn run_publication(arguments: PublishArgs) -> Result<(), phoxal_project::Error> {
    let (kind, package) = match arguments.package {
        PublishPackage::Component(package) => (PublicationKind::Component, package),
        PublishPackage::Service(package) => (PublicationKind::Service, package),
    };
    let result = prepare_publication(&PublicationOptions {
        kind,
        name: package.name,
        path: package.path,
        dry_run: package.dry_run,
    })?;
    println!("publication: dry-run");
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
    Ok(())
}

fn run_cargo(
    project: &Project,
    operation: CargoOperation,
    options: CargoOptions,
) -> Result<(), phoxal_project::Error> {
    let prepared = project.prepare(&options)?;
    for output in prepared.run(operation, &options)? {
        print_bytes(&output.stdout, false);
        print_bytes(&output.stderr, true);
    }
    Ok(())
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

#[derive(Debug, Subcommand)]
enum Command {
    /// Prepare the project, validate composition, and Cargo-check selected targets.
    Check(CommandArgs),
    /// Prepare the project, validate composition, and build selected targets.
    Build(BuildArgs),
    /// Prepare the project and run tests for the root robot package.
    Test(TestArgs),
    /// Prepare an authored component or service package for registry review.
    Publish(PublishArgs),
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
    /// Required until the remote registry submission client is available.
    #[arg(long, required = true)]
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
    /// Cargo compiler message format, such as `json`.
    #[arg(long)]
    message_format: Option<String>,
}

impl CommonArgs {
    fn into_options(self, cargo_args: Vec<OsString>, test_args: Vec<OsString>) -> CargoOptions {
        CargoOptions {
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
            message_format: self.message_format,
            cargo_args,
            test_args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
    fn publication_requires_explicit_dry_run_until_submission_exists() {
        let parsed = Cli::try_parse_from([
            "cargo-phoxal",
            "publish",
            "component",
            "example-passive-caster",
            "--dry-run",
        ])
        .expect("publication dry-run parses");
        assert!(matches!(parsed.command, Command::Publish(_)));
        assert!(
            Cli::try_parse_from([
                "cargo-phoxal",
                "publish",
                "component",
                "example-passive-caster",
            ])
            .is_err()
        );
    }
}
