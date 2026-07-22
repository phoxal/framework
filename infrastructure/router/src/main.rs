use std::io::{BufWriter, Write};
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use phoxal::infrastructure::router::RouterReadyV0;

const MAX_READY_FAILURE_BYTES: usize = 8 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "phoxal-infrastructure-router",
    about = "Run the Phoxal-owned Zenoh router"
)]
struct Args {
    /// Resolved Zenoh JSON5 configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Exact project-local Unix endpoint that every local participant uses.
    #[arg(long, value_name = "UNIX_ENDPOINT")]
    local_endpoint: String,

    /// Inherited descriptor receiving exactly one typed readiness result.
    #[cfg(unix)]
    #[arg(long, value_name = "FD")]
    ready_fd: RawFd,
}

struct OpenedRouter {
    session: zenoh::Session,
    local_endpoint: String,
    listeners: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let args = Args::parse();
    #[cfg(unix)]
    let ready_fd = args.ready_fd;
    let opened = match open_router(args).await {
        Ok(opened) => opened,
        Err(error) => {
            #[cfg(unix)]
            if let Err(report_error) = write_ready_result(
                ready_fd,
                &RouterReadyV0::Failed {
                    message: bounded_failure(format!("{error:#}")),
                },
            ) {
                tracing::warn!(
                    error = %report_error,
                    "failed to report router startup failure to supervisor"
                );
            }
            return Err(error);
        }
    };

    #[cfg(unix)]
    write_ready_result(
        ready_fd,
        &RouterReadyV0::Ready {
            local_endpoint: opened.local_endpoint.clone(),
            listeners: opened.listeners.clone(),
        },
    )?;
    tracing::info!(listeners = ?opened.listeners, "Phoxal infrastructure router ready");

    shutdown_signal().await?;
    opened
        .session
        .close()
        .await
        .map_err(|error| anyhow::anyhow!("failed to close Zenoh router: {error}"))?;
    Ok(())
}

fn init_tracing() -> Result<()> {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize router tracing: {error}"))?;
    Ok(())
}

async fn open_router(args: Args) -> Result<OpenedRouter> {
    validate_local_endpoint(&args.local_endpoint)?;
    ensure_local_endpoint_available(&args.local_endpoint)?;
    let authored = args.config.is_some();
    let mut base = load_config(args.config.as_deref())?;
    let authored_listeners = args
        .config
        .as_deref()
        .map(config_authors_listeners)
        .transpose()?
        .unwrap_or(false);
    force_router_mode(&mut base)?;
    // Authored configs retain their connect and scouting settings; the
    // loopback listener gate below still applies to every TCP listener.
    if !authored {
        apply_no_config_defaults(&mut base)?;
    }

    let mut listeners = if authored && authored_listeners {
        configured_listeners(&base)?
    } else {
        Vec::new()
    };
    if !listeners.contains(&args.local_endpoint) {
        listeners.insert(0, args.local_endpoint.clone());
    }
    validate_listeners(&listeners)?;
    let session = open_exact(base, &listeners).await?;
    Ok(OpenedRouter {
        session,
        local_endpoint: args.local_endpoint,
        listeners,
    })
}

fn load_config(path: Option<&Path>) -> Result<zenoh::Config> {
    match path {
        Some(path) => zenoh::Config::from_file(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to load Zenoh JSON5 config {}: {error}",
                path.display()
            )
        }),
        None => Ok(zenoh::Config::default()),
    }
}

/// Zenoh materializes mode-dependent listener defaults while parsing a config,
/// so inspect the validated source to distinguish an authored listener from an
/// inherited wildcard default. The source is parsed only for key presence;
/// Zenoh remains the authority for the configuration shape and values.
fn config_authors_listeners(path: &Path) -> Result<bool> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read Zenoh JSON5 config {}", path.display()))?;
    let value: serde_json::Value = json5::from_str(&source)
        .with_context(|| format!("failed to inspect Zenoh JSON5 config {}", path.display()))?;
    Ok(value
        .get("listen")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|listen| listen.contains_key("endpoints")))
}

fn force_router_mode(config: &mut zenoh::Config) -> Result<()> {
    insert(config, "mode", r#""router""#)
}

fn apply_no_config_defaults(config: &mut zenoh::Config) -> Result<()> {
    // Leave listener selection to the bounded allocator below instead of
    // inheriting Zenoh's router-mode wildcard default.
    insert(config, "listen/endpoints", "[]")?;
    insert(config, "connect/endpoints", "[]")?;
    insert(config, "scouting/multicast/enabled", "false")?;
    insert(config, "scouting/gossip/enabled", "false")?;
    insert(
        config,
        "scouting/multicast/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    insert(
        config,
        "scouting/gossip/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    Ok(())
}

fn configured_listeners(config: &zenoh::Config) -> Result<Vec<String>> {
    let json = config
        .get_json("listen/endpoints")
        .map_err(|error| anyhow::anyhow!("failed to read Zenoh listen endpoints: {error}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&json).context("Zenoh listen endpoints are not valid JSON")?;
    let endpoints = match value {
        serde_json::Value::Array(endpoints) => endpoints,
        serde_json::Value::Object(mut modes) => modes
            .remove("router")
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()))
            .as_array()
            .cloned()
            .context("Zenoh router-mode listen endpoints are not an array")?,
        _ => bail!("Zenoh listen endpoints are not an array or mode map"),
    };
    endpoints
        .into_iter()
        .map(|endpoint| {
            endpoint
                .as_str()
                .map(str::to_string)
                .context("Zenoh listen endpoint is not a string")
        })
        .collect()
}

async fn open_exact(mut config: zenoh::Config, listeners: &[String]) -> Result<zenoh::Session> {
    validate_listeners(listeners)?;
    insert_json(&mut config, "listen/endpoints", listeners)?;
    insert(&mut config, "listen/exit_on_failure", "true")?;
    force_router_mode(&mut config)?;
    zenoh::open(config).await.map_err(|error| {
        anyhow::anyhow!(
            "failed to open Zenoh router on {}: {error}",
            listeners.join(", ")
        )
    })
}

fn validate_listeners(listeners: &[String]) -> Result<()> {
    for listener in listeners {
        let listener = listener.trim();
        if listener.is_empty() {
            bail!("router listen endpoint must not be empty");
        }
        if let Some((scheme, endpoint)) = listener.split_once('/')
            && scheme.eq_ignore_ascii_case("tcp")
            && !tcp_endpoint_is_loopback(endpoint)
        {
            bail!(
                "router TCP listener '{listener}' must bind loopback until listener authentication ships"
            );
        }
    }
    Ok(())
}

fn validate_local_endpoint(endpoint: &str) -> Result<()> {
    let Some(path) = endpoint.strip_prefix("unixsock-stream/") else {
        bail!("router local endpoint '{endpoint}' must use the unixsock-stream transport");
    };
    if path.is_empty() || !Path::new(path).is_absolute() {
        bail!("router local unixsock-stream endpoint must contain an absolute path");
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_local_endpoint_available(endpoint: &str) -> Result<()> {
    let path = endpoint
        .strip_prefix("unixsock-stream/")
        .expect("validated local endpoint");
    if Path::new(path).exists() && std::os::unix::net::UnixStream::connect(path).is_ok() {
        bail!("router local endpoint '{endpoint}' is already accepting connections");
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_local_endpoint_available(_endpoint: &str) -> Result<()> {
    bail!("router local unix endpoint is unsupported on this platform")
}

fn tcp_endpoint_is_loopback(endpoint: &str) -> bool {
    let endpoint = endpoint
        .split_once('#')
        .map_or(endpoint, |(value, _)| value);
    let host = endpoint
        .strip_prefix('[')
        .and_then(|tail| tail.split_once(']').map(|(host, _rest)| host))
        .or_else(|| endpoint.rsplit_once(':').map(|(host, _port)| host))
        .unwrap_or(endpoint);

    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn insert(config: &mut zenoh::Config, path: &str, json5: &str) -> Result<()> {
    config
        .insert_json5(path, json5)
        .map_err(|error| anyhow::anyhow!("failed to set Zenoh config path {path}: {error}"))
}

fn insert_json<T: serde::Serialize + ?Sized>(
    config: &mut zenoh::Config,
    path: &str,
    value: &T,
) -> Result<()> {
    insert(config, path, &serde_json::to_string(value)?)
}

fn bounded_failure(mut message: String) -> String {
    if message.len() > MAX_READY_FAILURE_BYTES {
        let mut end = MAX_READY_FAILURE_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

#[cfg(unix)]
fn write_ready_result(fd: RawFd, result: &RouterReadyV0) -> Result<()> {
    // SAFETY: the supervisor transfers ownership of this inherited descriptor
    // to the router. This function consumes it exactly once and closes it after
    // the one-shot result is flushed.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, result)
        .context("failed to encode router readiness result")?;
    writer
        .write_all(b"\n")
        .context("failed to write router readiness delimiter")?;
    writer
        .flush()
        .context("failed to flush router readiness result")
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to install SIGTERM handler")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("failed to wait for Ctrl-C")?;
        }
        _ = terminate.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for Ctrl-C")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    #[cfg(unix)]
    use std::os::fd::IntoRawFd;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    use super::*;

    fn args(local_endpoint: String) -> Args {
        Args {
            config: None,
            local_endpoint,
            #[cfg(unix)]
            ready_fd: -1,
        }
    }

    #[test]
    fn forced_router_mode_overrides_authored_mode() {
        let mut config = zenoh::Config::from_json5(r#"{ mode: "peer" }"#).expect("config");
        force_router_mode(&mut config).expect("force mode");
        assert_eq!(config.get_json("mode").expect("mode"), r#""router""#);
    }

    #[test]
    fn no_config_defaults_are_loopback_only() {
        let mut config = zenoh::Config::default();
        apply_no_config_defaults(&mut config).expect("safe defaults");
        assert_eq!(config.get_json("listen/endpoints").expect("listen"), "[]");
        assert_eq!(config.get_json("connect/endpoints").expect("connect"), "[]");
        assert_eq!(
            config
                .get_json("scouting/multicast/enabled")
                .expect("multicast"),
            "false"
        );
        assert_eq!(
            config.get_json("scouting/gossip/enabled").expect("gossip"),
            "false"
        );
    }

    #[test]
    fn listener_gate_keeps_tcp_on_loopback_until_authentication_ships() {
        for allowed in [
            "tcp/localhost:7447",
            "tcp/127.0.0.1:7447",
            "tcp/127.2.3.4:7447",
            "tcp/[::1]:7447",
            "tls/0.0.0.0:7447",
        ] {
            validate_listeners(&[allowed.to_string()]).expect(allowed);
        }
        for rejected in [
            "tcp/0.0.0.0:7447",
            "tcp/192.0.2.1:7447",
            "tcp/[::]:7447",
            "tcp/127.example.com:7447",
            "TCP/0.0.0.0:7447",
        ] {
            let error = validate_listeners(&[rejected.to_string()]).expect_err(rejected);
            assert!(
                error
                    .to_string()
                    .contains("until listener authentication ships")
            );
        }
    }

    #[test]
    fn authored_config_listeners_use_the_same_loopback_gate() {
        let file = tempfile::Builder::new()
            .suffix(".json5")
            .tempfile()
            .expect("temporary config");
        std::fs::write(
            file.path(),
            r#"{ listen: { endpoints: ["tcp/0.0.0.0:7447"] } }"#,
        )
        .expect("write config");

        assert!(config_authors_listeners(file.path()).expect("inspect config"));
        let config = load_config(Some(file.path())).expect("load config");
        let listeners = configured_listeners(&config).expect("configured listeners");
        let error = validate_listeners(&listeners).expect_err("non-loopback TCP must fail");
        assert!(
            error
                .to_string()
                .contains("until listener authentication ships")
        );
    }

    #[test]
    fn authored_mode_dependent_listeners_select_router_mode() {
        let config = zenoh::Config::from_json5(
            r#"{
                listen: {
                    endpoints: {
                        router: ["tcp/127.0.0.1:7449"],
                        peer: ["tcp/127.0.0.1:7450"],
                        client: []
                    }
                }
            }"#,
        )
        .expect("config");

        assert_eq!(
            configured_listeners(&config).expect("router listeners"),
            vec!["tcp/127.0.0.1:7449".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authored_config_preserves_external_settings_beside_exact_local_endpoint() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let local = format!(
            "unixsock-stream/{}",
            dir.path().join("zenoh.sock").display()
        );
        let file = tempfile::Builder::new()
            .suffix(".json5")
            .tempfile()
            .expect("temporary config");
        std::fs::write(
            file.path(),
            r#"{
                mode: "peer",
                listen: { endpoints: ["tcp/127.0.0.1:0"] },
                connect: { endpoints: ["tcp/example.invalid:7447"] },
                scouting: {
                    multicast: { enabled: false },
                    gossip: { enabled: false }
                }
            }"#,
        )
        .expect("write config");

        let opened = open_router(Args {
            config: Some(file.path().to_path_buf()),
            local_endpoint: local.clone(),
            #[cfg(unix)]
            ready_fd: -1,
        })
        .await
        .expect("authored config should retain external listener");
        assert_eq!(opened.local_endpoint, local);
        assert_eq!(opened.listeners.len(), 2);
        assert_eq!(opened.listeners[0], opened.local_endpoint);
        assert_eq!(opened.listeners[1], "tcp/127.0.0.1:0");
        opened.session.close().await.expect("close router");
    }

    #[test]
    fn local_endpoint_must_be_an_absolute_unix_socket() {
        validate_local_endpoint("unixsock-stream//tmp/project/zenoh.sock").expect("absolute UDS");
        for invalid in [
            "tcp/127.0.0.1:7447",
            "unixsock-stream/relative.sock",
            "unixsock-stream/",
        ] {
            validate_local_endpoint(invalid).expect_err(invalid);
        }
    }

    #[cfg(unix)]
    #[test]
    fn readiness_result_is_typed_bounded_and_one_shot() {
        let (read, write) = UnixStream::pair().expect("socket pair");
        let expected = RouterReadyV0::Ready {
            local_endpoint: "unixsock-stream//tmp/project/zenoh.sock".to_string(),
            listeners: vec!["unixsock-stream//tmp/project/zenoh.sock".to_string()],
        };
        write_ready_result(write.into_raw_fd(), &expected).expect("write readiness");

        let mut lines = BufReader::new(read).lines();
        let line = lines.next().expect("one result").expect("read result");
        let decoded: RouterReadyV0 = serde_json::from_str(&line).expect("typed result");
        assert_eq!(decoded, expected);
        assert!(lines.next().is_none(), "writer must close after one result");

        let bounded = bounded_failure("é".repeat(MAX_READY_FAILURE_BYTES));
        assert!(bounded.len() <= MAX_READY_FAILURE_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[cfg(unix)]
    #[test]
    fn failed_readiness_result_round_trips_on_the_one_shot_descriptor() {
        let (read, write) = UnixStream::pair().expect("socket pair");
        let expected = RouterReadyV0::Failed {
            message: bounded_failure("bind failed: occupied endpoint".to_string()),
        };
        write_ready_result(write.into_raw_fd(), &expected).expect("write failed readiness");

        let mut lines = BufReader::new(read).lines();
        let line = lines.next().expect("one result").expect("read result");
        let decoded: RouterReadyV0 = serde_json::from_str(&line).expect("typed failed result");
        assert_eq!(decoded, expected);
        assert!(lines.next().is_none(), "writer must close after one result");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn occupied_local_endpoint_fails_instead_of_moving() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("zenoh.sock");
        let _occupied = std::os::unix::net::UnixListener::bind(&path).expect("occupy socket");
        let endpoint = format!("unixsock-stream/{}", path.display());
        let error = match open_router(args(endpoint.clone())).await {
            Ok(_) => panic!("exact local listener must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("already accepting connections"),
            "{message}"
        );
        assert!(message.contains(&endpoint), "{message}");
    }
}
