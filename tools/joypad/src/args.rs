use clap::Parser;
use phoxal::runtime::DEFAULT_ROBOT_NAMESPACE;
use phoxal::runtime::{
    ENV_ROBOT_CONNECT_RETRIES, ENV_ROBOT_CONNECT_TIMEOUT_MS, ENV_ROBOT_NAMESPACE,
    ENV_ROBOT_ROUTER_ENDPOINT, ENV_ROBOT_SIMULATION,
};
use phoxal::util::parse_trimmed_non_empty;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerSelectionMode {
    Interactive,
    Auto,
    Id(String),
}

fn parse_controller_selection(value: &str) -> Result<ControllerSelectionMode, String> {
    let value = parse_trimmed_non_empty(value)?;
    if value.eq_ignore_ascii_case("auto") {
        return Ok(ControllerSelectionMode::Auto);
    }

    if value.eq_ignore_ascii_case("interactive") {
        return Ok(ControllerSelectionMode::Interactive);
    }

    Ok(ControllerSelectionMode::Id(value))
}

#[derive(Debug, Parser)]
#[command(
    name = "joypad",
    about = "Host-side operator tool that maps a selected controller to drive commands."
)]
pub struct Args {
    #[arg(
        long,
        env = ENV_ROBOT_NAMESPACE,
        default_value_t = String::from(DEFAULT_ROBOT_NAMESPACE),
        value_parser = parse_trimmed_non_empty
    )]
    pub robot_namespace: String,

    #[arg(
        long = "router-endpoint",
        env = ENV_ROBOT_ROUTER_ENDPOINT
    )]
    pub router_endpoint: String,

    #[arg(long, env = ENV_ROBOT_SIMULATION, default_value_t = false)]
    pub simulation: bool,

    #[arg(
        long = "robot-connect-timeout-ms",
        env = ENV_ROBOT_CONNECT_TIMEOUT_MS,
        default_value_t = 60_000_u64
    )]
    pub robot_connect_timeout_ms: u64,

    #[arg(
        long = "robot-connect-retries",
        env = ENV_ROBOT_CONNECT_RETRIES,
        default_value_t = 5_u32
    )]
    pub robot_connect_retries: u32,

    #[arg(
        long = "controller",
        value_name = "auto|id",
        num_args = 0..=1,
        default_missing_value = "auto",
        default_value = "interactive",
        value_parser = parse_controller_selection
    )]
    pub controller: ControllerSelectionMode,

    #[arg(long = "xtask-session", hide = true)]
    pub xtask_session: Option<String>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Args, ControllerSelectionMode};

    #[test]
    fn controller_flag_without_value_defaults_to_auto() {
        let args = Args::parse_from([
            "joypad",
            "--router-endpoint",
            "tcp/127.0.0.1:7447",
            "--controller",
        ]);

        assert_eq!(args.controller, ControllerSelectionMode::Auto);
    }

    #[test]
    fn controller_flag_accepts_explicit_id() {
        let args = Args::parse_from([
            "joypad",
            "--router-endpoint",
            "tcp/127.0.0.1:7447",
            "--controller=2d931510-d99f-494a-8c67-87feb05e1594",
        ]);

        assert_eq!(
            args.controller,
            ControllerSelectionMode::Id("2d931510-d99f-494a-8c67-87feb05e1594".to_string())
        );
    }

    #[test]
    fn controller_defaults_to_interactive() {
        let args = Args::parse_from(["joypad", "--router-endpoint", "tcp/127.0.0.1:7447"]);

        assert_eq!(args.controller, ControllerSelectionMode::Interactive);
    }
}
