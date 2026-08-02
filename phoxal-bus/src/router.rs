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
//! [`crate::session::apply_phoxal_transport_policy`], which both paths share.
//! It is behind the `router` feature so participant authors, who reach the bus
//! through the `phoxal` facade, never see a router in their API surface: only
//! the supervisor enables it.

use std::path::Path;

use crate::error::{BusError, Result};
use crate::session::apply_phoxal_transport_policy;

/// A running Zenoh router session.
///
/// The router owns no keys, publishes nothing, and subscribes to nothing. It
/// routes, and it stays alive until dropped or [`Router::close`]d. Participants,
/// including the supervisor's own [`crate::Bus`], reach it as ordinary clients
/// over the endpoints it listens on.
#[derive(Debug)]
pub struct Router {
    session: zenoh::Session,
}

impl Router {
    /// Open a router listening on `listen_endpoints`.
    ///
    /// `config_file` is an optional native Zenoh JSON5 file supplying authored
    /// defaults. Phoxal's transport policy and the mode/listen settings are
    /// applied *after* it, so an authored file can tune what Phoxal does not
    /// pin but cannot silently put the router at odds with its clients.
    ///
    /// Returning `Ok` means the router is listening: Zenoh has bound every
    /// endpoint. There is no readiness probe to run afterwards and no window in
    /// which the endpoint exists but does not accept. [`router_config`] pins
    /// the settings that guarantee this, so an authored file cannot weaken it.
    pub async fn open(listen_endpoints: &[String], config_file: Option<&Path>) -> Result<Self> {
        if listen_endpoints.is_empty() {
            return Err(BusError::Transport(
                "a router needs at least one listen endpoint".to_string(),
            ));
        }
        let session = zenoh::open(router_config(listen_endpoints, config_file)?)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
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

    /// Observe transport links to this router opening and closing.
    ///
    /// One event per link, not per participant: a participant that reconnects
    /// produces a [`RouterLinkChange::Closed`] then a
    /// [`RouterLinkChange::Opened`]. This is transport-level truth, below the
    /// participant Liveliness that [`crate::Bus`] exposes, so it sees a peer
    /// dropping its connection immediately rather than after a lease expires.
    ///
    /// The returned observer stops delivering when dropped.
    pub async fn observe_links(
        &self,
        callback: impl Fn(RouterLinkEvent) + Send + Sync + 'static,
    ) -> Result<RouterLinkObserver> {
        let listener = self
            .session
            .info()
            .link_events_listener()
            .history(true)
            .callback(move |event| {
                callback(RouterLinkEvent {
                    change: match event.kind() {
                        zenoh::sample::SampleKind::Put => RouterLinkChange::Opened,
                        zenoh::sample::SampleKind::Delete => RouterLinkChange::Closed,
                    },
                    // Zenoh names these from the local session's point of
                    // view: `src` is this router's own endpoint, `dst` is the
                    // far side. Do not swap these back.
                    peer: event.link().dst().to_string(),
                    local: event.link().src().to_string(),
                });
            })
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        Ok(RouterLinkObserver {
            _listener: listener,
        })
    }

    /// Close the router, dropping every link to it.
    pub async fn close(self) -> Result<()> {
        self.session
            .close()
            .await
            .map_err(|error| BusError::Transport(error.to_string()))
    }
}

/// Whether a link appeared or went away.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouterLinkChange {
    Opened,
    Closed,
}

/// One transport link to the router opening or closing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouterLinkEvent {
    pub change: RouterLinkChange,
    /// The remote side of the link.
    pub peer: String,
    /// The router-side locator the link landed on - which of the router's
    /// endpoints this peer came in through.
    pub local: String,
}

/// Keeps [`Router::observe_links`] delivering. Dropping it stops the callback.
#[derive(Debug)]
pub struct RouterLinkObserver {
    _listener: zenoh::session::LinkEventsListener<()>,
}

fn router_config(listen_endpoints: &[String], config_file: Option<&Path>) -> Result<zenoh::Config> {
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
    // Everything below is applied after the authored file precisely so a file
    // cannot weaken it. The three `listen/*` keys are what make `open`'s
    // success mean "bound", which is the guarantee that lets the supervisor
    // delete its readiness probe: with a nonzero `timeout_ms` or
    // `exit_on_failure: false`, Zenoh moves binding to a background retry task
    // and `open` returns `Ok` with nothing listening. `scouting/delay` is a
    // flat sleep at the end of router startup, paid even though Phoxal keeps
    // multicast scouting off, so it is pure startup latency here.
    for (key, value) in [
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

    #[test]
    fn router_config_pins_mode_and_listen_endpoints() {
        let config =
            router_config(&["tcp/127.0.0.1:7447".to_string()], None).expect("router config");
        assert_eq!(config.get_json("mode").expect("mode is set"), "\"router\"");
        assert_eq!(
            config
                .get_json("listen/endpoints")
                .expect("listen endpoints are set"),
            "[\"tcp/127.0.0.1:7447\"]"
        );
    }

    #[test]
    fn router_config_carries_the_same_transport_policy_as_a_client() {
        // The whole reason this lives in phoxal-bus: both ends of a link must
        // agree, so assert it rather than trusting the call order.
        let config =
            router_config(&["tcp/127.0.0.1:7447".to_string()], None).expect("router config");
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

        let config = router_config(&["tcp/127.0.0.1:7447".to_string()], Some(&path))
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

        let config = router_config(&["tcp/127.0.0.1:7447".to_string()], Some(&path))
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
            &["tcp/127.0.0.1:7447".to_string()],
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
        let error = Router::open(&[], None)
            .await
            .expect_err("a router with nowhere to listen must fail");
        assert!(error.to_string().contains("listen endpoint"));
    }
}
