use std::path::PathBuf;
use std::process::ExitCode;

use phoxal_mujoco::{ClosedModel, Model, Scene};

#[derive(Debug)]
struct Options {
    model_path: PathBuf,
    steps: Option<u64>,
    duration_seconds: Option<f64>,
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
    let artifact = ClosedModel::from_file(&options.model_path).map_err(|error| error.to_string())?;
    let model = Model::from_closed(artifact).map_err(|error| error.to_string())?;
    let mut scene = Scene::new(model).map_err(|error| error.to_string())?;
    let steps = match (options.steps, options.duration_seconds) {
        (Some(steps), None) => steps,
        (None, Some(duration)) => {
            let exact_steps = duration / scene.quantum().as_seconds();
            let rounded_steps = exact_steps.round();
            if !exact_steps.is_finite()
                || rounded_steps < 1.0
                || (exact_steps - rounded_steps).abs() > 1.0e-9
                || rounded_steps > u64::MAX as f64
            {
                return Err(format!(
                    "duration {duration} is not an integral number of native quanta ({})",
                    scene.quantum().as_seconds()
                ));
            }
            rounded_steps as u64
        }
        (None, None) => 1,
        (Some(_), Some(_)) => return Err("choose either --steps or --duration".to_owned()),
    };
    let result = scene.advance(steps).map_err(|error| error.to_string())?;
    let state = result.state;
    println!(
        "{{\"model_id\":\"{}\",\"native_version\":\"{}\",\"boundary\":{},\"time_seconds\":{:.17},\"qpos_len\":{},\"sensor_data_len\":{}}}",
        scene.model().identity(),
        Model::native_version(),
        state.boundary(),
        state.time_seconds(),
        state.qpos().len(),
        state.sensor_data().len(),
    );
    Ok(())
}

impl Options {
    fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut model_path = None;
        let mut steps = None;
        let mut duration_seconds = None;
        let mut args = args.peekable();
        while let Some(argument) = args.next() {
            let argument = argument
                .into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())?;
            match argument.as_str() {
                "--help" | "-h" => {
                    println!("usage: phoxal-simulator-mujoco <model.xml> [--steps N | --duration SECONDS]");
                    return Err(String::new());
                }
                "--headless" => {}
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
        if let Some(duration) = duration_seconds
            && (!duration.is_finite() || duration <= 0.0)
        {
            return Err("--duration requires a positive finite number".to_owned());
        }
        Ok(Self {
            model_path,
            steps,
            duration_seconds,
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
        let options = parse(&["fixture.xml", "--headless", "--steps", "12"]).unwrap();
        assert_eq!(options.model_path, PathBuf::from("fixture.xml"));
        assert_eq!(options.steps, Some(12));
        assert_eq!(options.duration_seconds, None);
    }

    #[test]
    fn options_reject_conflicting_or_invalid_bounds() {
        assert!(parse(&["fixture.xml", "--steps", "0"]).is_err());
        assert!(parse(&["fixture.xml", "--steps", "2", "--duration", "0.02"]).is_err());
        assert!(parse(&["fixture.xml", "--duration", "NaN"]).is_err());
    }

    #[test]
    fn options_require_one_model_path() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["one.xml", "two.xml"]).is_err());
    }
}
