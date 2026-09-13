//! Loopback router for protocol conformance tests.

#[allow(clippy::expect_used, reason = "test fixture setup")]
pub async fn open(endpoints: &[String]) -> zenoh::Session {
    let mut config = zenoh::Config::default();
    for (key, value) in [
        ("mode", "\"router\"".to_owned()),
        (
            "listen/endpoints",
            serde_json::to_string(endpoints).expect("test endpoints"),
        ),
        ("listen/timeout_ms", "0".to_owned()),
        ("listen/exit_on_failure", "true".to_owned()),
        ("scouting/multicast/enabled", "false".to_owned()),
        ("scouting/gossip/enabled", "false".to_owned()),
        ("scouting/delay", "0".to_owned()),
    ] {
        config
            .insert_json5(key, &value)
            .expect("test router config");
    }
    zenoh::open(config).await.expect("test router")
}
