use std::path::PathBuf;
use std::process::ExitCode;

use phoxal_simulator_mujoco::core::{RunBounds, RunOutcome, SimulationCore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presentation {
    Headless,
    Desktop,
}

#[derive(Debug)]
struct Options {
    model_path: PathBuf,
    bounds: RunBounds,
    presentation: Presentation,
    native_core: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_empty() => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("phoxal-simulator-mujoco: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = Options::parse(std::env::args_os().skip(1))?;
    if !options.native_core {
        return Err(
            "a model-only invocation is disabled; use `--native-core` for a native smoke run or `cargo phoxal simulation run` for a bundle-backed run"
                .to_owned(),
        );
    }
    let mut core = SimulationCore::from_model_path(&options.model_path)
        .map_err(|error| error.to_string())?;
    match options.presentation {
        Presentation::Headless => {
            let summary = core
                .run_finite(options.bounds)
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&summary).map_err(|error| error.to_string())?
            );
            if summary.outcome == RunOutcome::Success {
                Ok(())
            } else {
                Err(summary
                    .failure
                    .map(|failure| failure.message)
                    .unwrap_or_else(|| "finite simulation did not complete".to_owned()))
            }
        }
        Presentation::Desktop => {
            #[cfg(feature = "desktop")]
            {
                phoxal_simulator_mujoco::desktop::run(core, options.bounds)
            }
            #[cfg(not(feature = "desktop"))]
            {
                let _ = core;
                Err("desktop presentation requires the `desktop` feature".to_owned())
            }
        }
    }
}

impl Options {
    fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut model_path = None;
        let mut steps = None;
        let mut duration_seconds = None;
        let mut presentation = None;
        let mut native_core = false;
        let mut args = args.peekable();
        while let Some(argument) = args.next() {
            let argument = argument
                .into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())?;
            match argument.as_str() {
                "--help" | "-h" => {
                    println!(
                        "usage: phoxal-simulator-mujoco <model.xml> --native-core [--headless|--desktop] [--steps N | --duration SECONDS]"
                    );
                    return Err(String::new());
                }
                "--headless" => {
                    if presentation.replace(Presentation::Headless).is_some_and(|value| {
                        value != Presentation::Headless
                    }) {
                        return Err("choose either --headless or --desktop".to_owned());
                    }
                }
                "--desktop" => {
                    if presentation.replace(Presentation::Desktop).is_some_and(|value| {
                        value != Presentation::Desktop
                    }) {
                        return Err("choose either --headless or --desktop".to_owned());
                    }
                }
                "--native-core" => {
                    if native_core {
                        return Err("--native-core may be specified once".to_owned());
                    }
                    native_core = true;
                }
                "--steps" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--steps requires a positive integer".to_owned())?
                        .into_string()
                        .map_err(|_| "--steps requires a positive integer".to_owned())?;
                    let parsed = value
                        .parse::<u64>()
                        .map_err(|_| "--steps requires a positive integer".to_owned())?;
                    if parsed == 0 {
                        return Err("--steps requires a positive integer".to_owned());
                    }
                    if steps.replace(parsed).is_some() {
                        return Err("--steps may be specified once".to_owned());
                    }
                }
                "--duration" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--duration requires a positive finite number".to_owned())?
                        .into_string()
                        .map_err(|_| "--duration requires a positive finite number".to_owned())?;
                    let parsed = value.parse::<f64>().map_err(|_| {
                        "--duration requires a positive finite number".to_owned()
                    })?;
                    if !parsed.is_finite() || parsed <= 0.0 {
                        return Err("--duration requires a positive finite number".to_owned());
                    }
                    if duration_seconds.replace(parsed).is_some() {
                        return Err("--duration may be specified once".to_owned());
                    }
                }
                value if value.starts_with('-') => {
                    return Err(format!("unknown option {value}"));
                }
                value => {
                    if model_path.replace(PathBuf::from(value)).is_some() {
                        return Err("only one model path may be specified".to_owned());
                    }
                }
            }
        }
        if steps.is_some() && duration_seconds.is_some() {
            return Err("choose either --steps or --duration".to_owned());
        }
        let model_path = model_path.ok_or_else(|| "a model path is required".to_owned())?;
        let bounds = match (steps, duration_seconds) {
            (Some(steps), None) => RunBounds::Steps(steps),
            (None, Some(duration)) => RunBounds::Duration(duration),
            (None, None) => RunBounds::Steps(1),
            (Some(_), Some(_)) => unreachable!("bounds conflict was checked above"),
        };
        Ok(Self {
            model_path,
            bounds,
            presentation: presentation.unwrap_or(Presentation::Headless),
            native_core,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Options, String> {
        Options::parse(arguments.iter().copied().map(std::ffi::OsString::from))
    }

    #[test]
    fn options_parse_exact_steps_and_headless_mode() {
        let options = parse(&[
            "fixture.xml",
            "--native-core",
            "--headless",
            "--steps",
            "12",
        ])
        .unwrap();
        assert_eq!(options.model_path, PathBuf::from("fixture.xml"));
        assert_eq!(options.bounds, RunBounds::Steps(12));
        assert_eq!(options.presentation, Presentation::Headless);
    }

    #[test]
    fn options_parse_desktop_and_exact_duration() {
        let options = parse(&[
            "fixture.xml",
            "--native-core",
            "--desktop",
            "--duration",
            "0.1",
        ])
        .unwrap();
        assert_eq!(options.bounds, RunBounds::Duration(0.1));
        assert_eq!(options.presentation, Presentation::Desktop);
    }

    #[test]
    fn options_reject_conflicting_or_invalid_bounds() {
        assert!(parse(&["fixture.xml", "--steps", "0"]).is_err());
        assert!(parse(&["fixture.xml", "--steps", "2", "--duration", "0.02"]).is_err());
        assert!(parse(&["fixture.xml", "--duration", "NaN"]).is_err());
        assert!(parse(&["fixture.xml", "--headless", "--desktop"]).is_err());
    }

    #[test]
    fn options_require_one_model_path() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["one.xml", "two.xml"]).is_err());
    }
}
