//! Transport helpers for crate integration tests.

/// Connects to one local supervisor endpoint and waits for an execution-scoped
/// participant Ready lease.
///
/// This keeps transport-specific test plumbing inside the bus owner while
/// integration fixtures exercise the real router and process boundary.
pub async fn wait_for_participant_ready(endpoint: &str, instance: &str) -> anyhow::Result<()> {
    let session = loop {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("mode", "\"client\"")
            .map_err(|error| anyhow::anyhow!("invalid client mode: {error}"))?;
        config
            .insert_json5("connect/endpoints", &serde_json::to_string(&[endpoint])?)
            .map_err(|error| anyhow::anyhow!("invalid connect endpoint: {error}"))?;
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .map_err(|error| anyhow::anyhow!("invalid scouting setting: {error}"))?;
        config
            .insert_json5("connect/timeout_ms", "250")
            .map_err(|error| anyhow::anyhow!("invalid connect timeout: {error}"))?;
        match zenoh::open(config).await {
            Ok(session) => break session,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    };
    let subscriber = session
        .liveliness()
        .declare_subscriber("phoxal/**")
        .history(true)
        .await
        .map_err(|error| anyhow::anyhow!("cannot observe participant liveliness: {error}"))?;
    loop {
        let sample = subscriber
            .recv_async()
            .await
            .map_err(|error| anyhow::anyhow!("participant liveliness observer ended: {error}"))?;
        if sample.kind() == zenoh::sample::SampleKind::Put
            && sample
                .key_expr()
                .as_str()
                .contains(&format!("/liveliness/participants/{instance}/"))
        {
            drop(subscriber);
            session
                .close()
                .await
                .map_err(|error| anyhow::anyhow!("cannot close observer session: {error}"))?;
            return Ok(());
        }
    }
}
