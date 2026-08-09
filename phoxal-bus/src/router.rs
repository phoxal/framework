//! The Phoxal-owned Zenoh router, opened in the supervisor's own process.
//!
//! This module exists because the comms fabric is not a robot participant: it
//! is infrastructure the supervisor firsthand owns, alongside process
//! supervision and lifecycle facts. Running it in-process removes a child
//! process whose only failure mode was "the fabric went away", which the
//! supervisor cannot survive anyway.
//!
//! It lives in `phoxal-bus` rather than in the CLI because the router and the
//! participants that dial it must agree on transport policy - see
//! `crate::session::apply_phoxal_transport_policy`, which both paths share.
//! It is behind the `router` feature so participant authors, who reach the bus
//! through the `phoxal` facade, never see a router in their API surface: only
//! the supervisor enables it.

use std::path::Path;

use phoxal_runtime_contract::identity::ExecutionId;

use crate::error::{BusError, Result};
use crate::session::{
    apply_phoxal_transport_policy, client_config, execution_from_zid, zenoh_id_for,
};

/// A running Zenoh router session.
///
/// The router owns no keys, publishes nothing, and subscribes to nothing. It
/// routes, and it stays alive until dropped or [`Router::close`]d. Participants,
/// including the supervisor's own [`crate::BusHandle`], reach it as ordinary clients
/// over the endpoints it listens on.
#[derive(Debug)]
pub struct Router {
    session: zenoh::Session,
}

impl Router {
    /// Open a router for `execution`, listening on `listen_endpoints`.
    ///
    /// The router's session id *is* the execution, so the fabric a trace names
    /// and the key root that trace carries are the same string. An authored
    /// `id` cannot win: the run it would name is not the run it is routing.
    ///
    /// `config_file` is an optional native Zenoh JSON5 file supplying authored
    /// defaults. Phoxal's transport policy, the session id, and the mode/listen
    /// settings are applied *after* it, so an authored file can tune what
    /// Phoxal does not pin but cannot silently put the router at odds with its
    /// clients.
    ///
    /// Returning `Ok` means the router is listening: Zenoh has bound every
    /// endpoint. There is no readiness probe to run afterwards and no window in
    /// which the endpoint exists but does not accept. `router_config` pins
    /// the settings that guarantee this, so an authored file cannot weaken it.
    pub async fn open(
        execution: ExecutionId,
        listen_endpoints: &[String],
        config_file: Option<&Path>,
    ) -> Result<Self> {
        if listen_endpoints.is_empty() {
            return Err(BusError::Transport(
                "a router needs at least one listen endpoint".to_string(),
            ));
        }
        let session = zenoh::open(router_config(execution, listen_endpoints, config_file)?)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let observed = match execution_from_zid(session.zid()) {
            Ok(observed) => observed,
            Err(error) => {
                let _ = session.close().await;
                return Err(error);
            }
        };
        if observed != execution {
            let _ = session.close().await;
            return Err(BusError::ExecutionIdentityMismatch {
                expected: execution,
                observed,
            });
        }
        Ok(Router { session })
    }

    /// The endpoints the router actually bound.
    ///
    /// This is the answer to "which port did I get". Asking to listen on
    /// `tcp/0.0.0.0:0` binds an ephemeral port chosen by the OS, and this is the
    /// only way to learn it - the requested endpoint string still says `:0`.
    /// Callers that pin a port get back what they asked for.
    pub async fn bound_endpoints(&self) -> Vec<String> {
        self.session
            .info()
            .locators()
            .await
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Close the router, dropping every link to it.
    pub async fn close(self) -> Result<()> {
        self.session
            .close()
            .await
            .map_err(|error| BusError::Transport(error.to_string()))
    }
}

/// Watches the supervisor's own link to the router it is running.
///
/// The router runs inside the supervisor process, so "is the router still
/// there" is not answerable from the router's own session - if that session
/// dies it takes its own listener with it. The answer comes from the other end:
/// a client session dialing the router sees exactly one link, and that link
/// going away *is* the router going away.
///
/// This is deliberately not built on participant links. Participants report
/// themselves through Liveliness, which names them; a transport link over the
/// local unix socket identifies its peer only by a generated UUID, so watching
/// those would duplicate Liveliness with strictly worse information.
#[derive(Debug)]
pub struct RouterWatch {
    session: zenoh::Session,
    _listener: zenoh::session::LinkEventsListener<()>,
}

impl RouterWatch {
    /// Dial `endpoint` and call `on_lost` if the link to the router goes away.
    ///
    /// Returning `Ok` means the link is up. `on_lost` fires at most once per
    /// loss; a Zenoh client reconnects transparently, so a later recovery is
    /// not reported here - the caller has already decided what a lost fabric
    /// means by then.
    pub async fn open(endpoint: &str, on_lost: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let session = zenoh::open(client_config(endpoint)?)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;

        let lost = std::sync::atomic::AtomicBool::new(false);
        let listener = session
            .info()
            .link_events_listener()
            .history(true)
            .callback(move |event| {
                if event.kind() == zenoh::sample::SampleKind::Delete
                    && !lost.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    on_lost();
                }
            })
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        Ok(RouterWatch {
            session,
            _listener: listener,
        })
    }

    /// Stop watching. Call this before closing the router being watched, so an
    /// ordinary shutdown is not reported as a loss.
    pub async fn close(self) -> Result<()> {
        self.session
            .close()
            .await
            .map_err(|error| BusError::Transport(error.to_string()))
    }
}

fn router_config(
    execution: ExecutionId,
    listen_endpoints: &[String],
    config_file: Option<&Path>,
) -> Result<zenoh::Config> {
    let mut config = match config_file {
        Some(path) => zenoh::Config::from_file(path).map_err(|error| {
            BusError::Transport(format!(
                "failed to read the router config at {}: {error}",
                path.display()
            ))
        })?,
        None => zenoh::Config::default(),
    };
    apply_phoxal_transport_policy(&mut config)?;
    let endpoints = serde_json::to_string(listen_endpoints)
        .map_err(|error| BusError::Transport(error.to_string()))?;
    // Rendered through the session-id conversion rather than from the
    // execution's own text, so this is the value Zenoh will report back, not a
    // string that merely looks like it.
    let id = serde_json::to_string(&zenoh_id_for(execution)?.to_string())
        .map_err(|error| BusError::Transport(error.to_string()))?;
    // Everything below is applied after the authored file precisely so a file
    // cannot weaken it. The three `listen/*` keys are what make `open`'s
    // success mean "bound", which is the guarantee that lets the supervisor
    // delete its readiness probe: with a nonzero `timeout_ms` or
    // `exit_on_failure: false`, Zenoh moves binding to a background retry task
    // and `open` returns `Ok` with nothing listening. `scouting/delay` is a
    // flat sleep at the end of router startup, paid even though Phoxal keeps
    // multicast scouting off, so it is pure startup latency here.
    for (key, value) in [
        ("id", id.as_str()),
        ("mode", "\"router\""),
        ("listen/endpoints", endpoints.as_str()),
        ("listen/timeout_ms", "0"),
        ("listen/exit_on_failure", "true"),
        ("scouting/delay", "0"),
    ] {
        config
            .insert_json5(key, value)
            .map_err(|error| BusError::Transport(error.to_string()))?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use serial_test::serial;

    use crate::contract::EndpointDescriptor;
    use crate::handle::publisher::StatePublisher;
    use crate::handle::subscriber::Latest;
    use crate::session::{BusConfig, BusOwner};
    use crate::test_support::{Target, TargetEndpoint, socket_endpoint, step};
    use crate::topic::{Publish, Subscribe, Topic};

    const ENDPOINT: &str = "tcp/127.0.0.1:7447";

    fn endpoints() -> Vec<String> {
        vec![ENDPOINT.to_string()]
    }

    #[test]
    fn router_config_pins_mode_and_listen_endpoints() {
        let config = router_config(ExecutionId::mint(), &endpoints(), None).expect("router config");
        assert_eq!(config.get_json("mode").expect("mode is set"), "\"router\"");
        assert_eq!(
            config
                .get_json("listen/endpoints")
                .expect("listen endpoints are set"),
            "[\"tcp/127.0.0.1:7447\"]"
        );
    }

    /// The router's session id is the execution it routes, and an authored file
    /// cannot rename the run.
    #[test]
    fn the_router_session_id_is_the_execution_and_an_authored_id_cannot_override_it() {
        let execution = ExecutionId::mint();
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("zenoh.json5");
        std::fs::write(&path, r#"{ id: "abcdef" }"#).expect("write authored router config");

        for authored in [None, Some(path.as_path())] {
            let config = router_config(execution, &endpoints(), authored).expect("router config");
            assert_eq!(
                config.get_json("id").expect("the session id is pinned"),
                format!("\"{execution}\""),
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_running_router_reports_the_execution_as_its_own_session_id() {
        let execution = ExecutionId::mint();
        let router = Router::open(execution, &["tcp/127.0.0.1:0".to_string()], None)
            .await
            .expect("the router binds an OS-assigned port");
        assert_eq!(router.session.zid().to_string(), execution.to_string());
        router.close().await.expect("the router closes");
    }

    #[test]
    fn router_config_carries_the_same_transport_policy_as_a_client() {
        // The whole reason this lives in phoxal-bus: both ends of a link must
        // agree, so assert it rather than trusting the call order.
        let config = router_config(ExecutionId::mint(), &endpoints(), None).expect("router config");
        assert_eq!(
            config
                .get_json("transport/link/tx/lease")
                .expect("lease is set"),
            "3000"
        );
        assert_eq!(
            config
                .get_json("transport/link/tx/keep_alive")
                .expect("keepalive is set"),
            "4"
        );
        assert_eq!(
            config
                .get_json("scouting/multicast/enabled")
                .expect("multicast is set"),
            "false"
        );
    }

    #[test]
    fn phoxal_policy_and_mode_override_an_authored_config_file() {
        // An authored file supplies defaults; it must not be able to put the
        // router at odds with the clients that dial it.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("zenoh.json5");
        std::fs::write(
            &path,
            r#"{ mode: "client", transport: { link: { tx: { lease: 9999 } } } }"#,
        )
        .expect("write authored router config");

        let config = router_config(ExecutionId::mint(), &endpoints(), Some(&path))
            .expect("authored router config");
        assert_eq!(config.get_json("mode").expect("mode is set"), "\"router\"");
        assert_eq!(
            config
                .get_json("transport/link/tx/lease")
                .expect("lease is set"),
            "3000",
            "Phoxal transport policy must win over an authored default"
        );
    }

    #[test]
    fn an_authored_file_cannot_weaken_the_bound_on_open_guarantee() {
        // `Router::open` returning `Ok` means "listening", and the supervisor
        // deletes its readiness probe on that promise. Zenoh only keeps it when
        // binding is synchronous and fatal: a nonzero `listen/timeout_ms` or
        // `exit_on_failure: false` moves binding to a background retry task and
        // `open` succeeds with nothing bound. An authored file must not be able
        // to reach that.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("zenoh.json5");
        std::fs::write(
            &path,
            r#"{ listen: { timeout_ms: 60000, exit_on_failure: false } }"#,
        )
        .expect("write authored router config");

        let config = router_config(ExecutionId::mint(), &endpoints(), Some(&path))
            .expect("authored router config");
        assert_eq!(
            config
                .get_json("listen/timeout_ms")
                .expect("listen timeout is pinned"),
            "0",
            "a background-retry bind would make `open` succeed with nothing listening"
        );
        assert_eq!(
            config
                .get_json("listen/exit_on_failure")
                .expect("listen exit_on_failure is pinned"),
            "true",
            "a bind failure must fail `open`, not be swallowed"
        );
    }

    #[test]
    fn a_missing_config_file_names_the_path_it_could_not_read() {
        let error = router_config(
            ExecutionId::mint(),
            &endpoints(),
            Some(Path::new("/nonexistent/zenoh.json5")),
        )
        .expect_err("a missing config file must fail");
        assert!(
            error.to_string().contains("/nonexistent/zenoh.json5"),
            "the error must name the unreadable path, got: {error}"
        );
    }

    #[tokio::test]
    async fn opening_without_a_listen_endpoint_is_rejected() {
        let error = Router::open(ExecutionId::mint(), &[], None)
            .await
            .expect_err("a router with nowhere to listen must fail");
        assert!(error.to_string().contains("listen endpoint"));
    }

    /// The load-bearing proof for the embedded router: two *separate* client
    /// sessions exchange a real sample, which can only happen if the in-process
    /// router actually relays between them.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_client_sessions_exchange_a_sample_through_an_embedded_router() {
        let (_dir, endpoint) = socket_endpoint("phoxal-embedded-router-");

        // One execution for the router and both clients, so the sessions compose
        // the same key root and the sample is observable rather than scoped away.
        let execution = ExecutionId::mint();
        let router = Router::open(execution, std::slice::from_ref(&endpoint), None)
            .await
            .expect("the embedded router binds its endpoint");

        let client = |execution: ExecutionId, participant: &str| {
            BusConfig::for_participant(
                execution,
                phoxal_runtime_contract::identity::ParticipantId::new(participant)
                    .expect("test participant id"),
                vec![endpoint.clone()],
            )
        };

        let (publishing_owner, publishing) = BusOwner::open(client(execution, "publisher"))
            .await
            .expect("publisher dials the embedded router");
        let (subscribing_owner, subscribing) = BusOwner::open(client(execution, "subscriber"))
            .await
            .expect("subscriber dials the embedded router");
        assert_ne!(
            publishing.producer(),
            subscribing.producer(),
            "the two sides must be genuinely separate sessions for this to prove relay"
        );

        let pub_topic = Topic::<Publish<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let sub_topic = Topic::<Subscribe<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let publisher =
            StatePublisher::<TargetEndpoint>::new(publishing.clone(), &pub_topic).unwrap();
        let latest = Latest::<TargetEndpoint>::new(&subscribing, &sub_topic)
            .await
            .unwrap();

        // Republish each round rather than publishing once: the subscriber's
        // declaration travels B -> router while the sample travels A -> router, and
        // `declare_subscriber` does not wait for the router to acknowledge it, so a
        // single publish has a small window in which it arrives before the
        // subscription exists and is dropped. Retrying removes the flake without
        // weakening the assertion.
        let mut observed = None;
        for tick in 0..100 {
            publisher
                .publish(
                    &step(1, 100 + tick),
                    Target {
                        linear_x_mps: 0.4,
                        angular_z_radps: 0.2,
                    },
                )
                .unwrap();
            if let Some(sample) = latest.observed() {
                observed = Some(sample);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let observed = observed.expect("the router must relay the sample to the other session");
        assert_eq!(observed.body.linear_x_mps, 0.4);
        assert_eq!(
            observed.metadata.source.producer(),
            publishing.producer(),
            "provenance must survive the hop through the router"
        );

        // A second execution shares the exact same physical router/fabric but
        // has a different execution-rooted key. It gets its own publisher and
        // subscriber so the test proves both isolation and same-root delivery.
        let other_execution = ExecutionId::mint();
        let (other_publishing_owner, other_publishing) =
            BusOwner::open(client(other_execution, "other-publisher"))
                .await
                .expect("the second execution publisher dials the same router");
        let (other_subscribing_owner, other_subscribing) =
            BusOwner::open(client(other_execution, "other-subscriber"))
                .await
                .expect("the second execution subscriber dials the same router");
        let other_publisher =
            StatePublisher::<TargetEndpoint>::new(other_publishing.clone(), &pub_topic).unwrap();
        let other_latest = Latest::<TargetEndpoint>::new(&other_subscribing, &sub_topic)
            .await
            .unwrap();

        assert!(other_latest.latest().is_none());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            other_latest.latest().is_none(),
            "a sample under one execution root must not reach another root on the same fabric"
        );

        let mut other_observed = None;
        for tick in 0..100 {
            other_publisher
                .publish(
                    &step(1, 200 + tick),
                    Target {
                        linear_x_mps: 0.8,
                        angular_z_radps: 0.3,
                    },
                )
                .unwrap();
            if let Some(sample) = other_latest.observed() {
                other_observed = Some(sample);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let other_observed =
            other_observed.expect("the second execution must exchange within its own root");
        assert_eq!(other_observed.body.linear_x_mps, 0.8);
        assert_eq!(
            other_observed.metadata.source.producer(),
            other_publishing.producer()
        );

        publishing_owner.close().await;
        subscribing_owner.close().await;
        other_publishing_owner.close().await;
        other_subscribing_owner.close().await;
        router.close().await.unwrap();
    }

    /// An ephemeral port is the whole reason `bound_endpoints` exists: the caller
    /// asks for `:0`, the OS picks, and the requested string still says `:0`.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ephemeral_port_is_reported_back_and_is_actually_connectable() {
        let execution = ExecutionId::mint();
        let router = Router::open(execution, &["tcp/127.0.0.1:0".to_string()], None)
            .await
            .expect("router binds an OS-assigned port");

        let bound = router.bound_endpoints().await;
        let tcp = bound
            .iter()
            .find(|endpoint| endpoint.starts_with("tcp/"))
            .expect("the router reports the tcp endpoint it bound");
        let port: u16 = tcp
            .rsplit(':')
            .next()
            .and_then(|port| port.parse().ok())
            .expect("the reported endpoint carries a concrete port");
        assert_ne!(
            port, 0,
            "`:0` must resolve to a real assigned port, got {tcp}"
        );

        // Reported is not the same as reachable - dial the port we were told.
        let (owner, _bus) = BusOwner::open(BusConfig::for_participant(
            execution,
            phoxal_runtime_contract::identity::ParticipantId::new("client")
                .expect("test participant id"),
            vec![tcp.clone()],
        ))
        .await
        .expect("the reported endpoint must be the one that actually accepts");

        owner.close().await;
        router.close().await.unwrap();
    }

    /// The supervisor's question is "is the router I am running still there", and
    /// the only end that can answer it is the client end. Prove the whole loop: a
    /// watch reports nothing while the router is up, and fires exactly once when it
    /// goes away.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_watch_fires_when_the_router_goes_away_and_not_before() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (_dir, endpoint) = socket_endpoint("phoxal-router-watch-");
        let router = Router::open(ExecutionId::mint(), std::slice::from_ref(&endpoint), None)
            .await
            .expect("router binds");

        let losses = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&losses);
        let watch = RouterWatch::open(&endpoint, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .expect("the watch dials the running router");

        // A healthy router must stay silent. `history(true)` replays the link that
        // already exists, so a watch that keyed off any event rather than a close
        // would fire here.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            losses.load(Ordering::Relaxed),
            0,
            "a live router must not be reported as lost"
        );

        router.close().await.expect("close the router");

        let mut fired = false;
        for _ in 0..100 {
            if losses.load(Ordering::Relaxed) > 0 {
                fired = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(fired, "losing the router must be reported");

        // Zenoh keeps retrying a dead endpoint; the loss is reported once, not per
        // failed reconnect.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            losses.load(Ordering::Relaxed),
            1,
            "the loss must be reported exactly once, not once per reconnect attempt"
        );

        watch.close().await.expect("close the watch");
    }
}
