use std::borrow::Cow;

use anyhow::{Result, ensure};
use phoxal::api::presence::v1::{DebugReadiness, Readiness};
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::helpers::{
    assert_ready_summary, heartbeat, readiness_for, readiness_summary,
};

const P0_RUNTIMES: &[&str] = &["asset", "power", "presence", "router"];

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("readiness-bootstrap"),
    summary: Cow::Borrowed("Checks presence readiness aggregation during P0 bootstrap."),
    kind: ScenarioKind::Headless,
    phase: phoxal::runtime::Phase::P0,
    timeout_secs: 60,
    category: Cow::Borrowed("readiness-bootstrap"),
    tier: 1,
}];

pub fn run(name: &str) -> Result<()> {
    match name {
        "readiness-bootstrap" => readiness_bootstrap(),
        _ => anyhow::bail!("presence has no scenario '{name}'"),
    }
}

fn readiness_bootstrap() -> Result<()> {
    let boot_ready = readiness_summary(
        P0_RUNTIMES
            .iter()
            .map(|runtime| heartbeat(runtime, Readiness::Ready))
            .collect(),
    );
    assert_ready_summary(&boot_ready, P0_RUNTIMES)?;
    ensure!(
        !boot_ready.autonomy_ready,
        "P0 boot contract must not mark autonomy ready"
    );

    let initializing = readiness_summary(vec![
        heartbeat("router", Readiness::Ready),
        heartbeat("asset", Readiness::Ready),
        heartbeat("presence", Readiness::Ready),
        heartbeat("power", Readiness::Initializing),
    ]);
    let debug = DebugReadiness {
        runtimes: initializing.runtimes.clone(),
    };

    ensure!(
        initializing.runtimes == debug.runtimes,
        "presence summary and debug readiness disagree during bootstrap"
    );
    ensure!(
        readiness_for(&initializing, "power") == Some(Readiness::Initializing),
        "power readiness did not surface as initializing"
    );
    ensure!(
        !initializing.autonomy_ready,
        "infra readiness alone must not arm autonomy"
    );

    let ready = readiness_summary(vec![
        heartbeat("router", Readiness::Ready),
        heartbeat("asset", Readiness::Ready),
        heartbeat("presence", Readiness::Ready),
        heartbeat("power", Readiness::Ready),
    ]);
    assert_ready_summary(&ready, P0_RUNTIMES)?;

    let degraded = readiness_summary(vec![
        heartbeat("router", Readiness::Ready),
        heartbeat("asset", Readiness::Ready),
        heartbeat("presence", Readiness::Ready),
        heartbeat("power", Readiness::Degraded),
    ]);
    ensure!(
        readiness_for(&degraded, "power") == Some(Readiness::Degraded),
        "degraded runtime readiness was not preserved"
    );

    Ok(())
}
