//! `localize` - the official odometry-anchored localization runtime.
//!
//! A scheduled runtime that subscribes to `odometry/state` and republishes
//! `localize/state`, the localization estimate consumed by downstream runtimes
//! such as `map`, `explore`, and `follow`.
//! This first version is transparent: the odometry pose is treated directly as
//! the map-frame pose, and confidence decays linearly to zero as the latest
//! odometry sample ages past the staleness window.
//! Until the first odometry fix arrives it publishes nothing, so downstream
//! runtimes never mistake a fabricated origin pose for a real estimate.
//! It does not implement ORB-SLAM3, visual-inertial localization, or GNSS
//! anchoring; map-frame and odometry-frame are assumed identical.

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

const LOCALIZE_STALE_NS: u64 = 1_000_000_000; // 1 s

#[derive(phoxal::Runtime)]
#[phoxal(id = "localize", api = y2026_1)]
struct Localize {
    // Runtime-private typed state (not handles).
    last_odometry: Option<(api::odometry::State, u64)>,
    // Handles.
    odometry: Subscriber<api::odometry::State>,
    state: Publisher<api::localize::LocalizationState>,
}

#[phoxal::runtime]
impl Localize {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let odometry = ctx
            .subscribe(api::topic::new().odometry().state())
            .subscriber()
            .await?;
        let state = ctx.publisher(api::topic::new().localize().state()).await?;

        Ok(Self {
            last_odometry: None,
            odometry,
            state,
        })
    }

    #[step(hz = 20)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.odometry.try_recv() {
            self.last_odometry = Some((received.body, received.metadata.produced_at_ns));
        }

        // Until the first odometry fix, publish nothing so downstream runtimes do
        // not mistake a fabricated origin pose for a valid localization estimate.
        let Some((odometry, produced_at_ns)) = &self.last_odometry else {
            return Ok(());
        };

        let confidence = confidence_for(step.time().time_ns().saturating_sub(*produced_at_ns));
        self.state
            .publish_at(step.time(), localization_from(odometry, confidence))
            .await?;
        Ok(())
    }
}

fn confidence_for(age_ns: u64) -> f32 {
    let age_fraction = age_ns as f64 / LOCALIZE_STALE_NS as f64;
    (1.0 - age_fraction).clamp(0.0, 1.0) as f32
}

fn localization_from(
    odometry: &api::odometry::State,
    confidence: f32,
) -> api::localize::LocalizationState {
    api::localize::LocalizationState {
        x_m: odometry.x_m,
        y_m: odometry.y_m,
        yaw_rad: odometry.yaw_rad,
        confidence,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Localize>()
}

#[cfg(test)]
mod tests {
    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

    use super::{LOCALIZE_STALE_NS, Localize, confidence_for, localization_from};

    #[test]
    fn confidence_decays_with_age() {
        assert_eq!(confidence_for(0), 1.0);
        assert_eq!(confidence_for(LOCALIZE_STALE_NS), 0.0);
        assert!((confidence_for(LOCALIZE_STALE_NS / 2) - 0.5).abs() < 1e-6);
        assert_eq!(confidence_for(LOCALIZE_STALE_NS + 1), 0.0);
    }

    #[test]
    fn localization_copies_odometry_pose() {
        let odometry = api::odometry::State {
            x_m: 1.25,
            y_m: -2.5,
            yaw_rad: 0.75,
            linear_x_mps: 0.4,
            angular_z_radps: -0.2,
        };

        let localization = localization_from(&odometry, 0.42);

        assert_eq!(localization.x_m, odometry.x_m);
        assert_eq!(localization.y_m, odometry.y_m);
        assert_eq!(localization.yaw_rad, odometry.yaw_rad);
        assert_eq!(localization.confidence, 0.42);
    }

    #[test]
    fn emit_apis_reports_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Localize>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "localize");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::odometry::State as ContractBody>::FAMILY
                && c["direction"] == "subscribe"
        }));
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::localize::LocalizationState as ContractBody>::FAMILY
                && c["direction"] == "publish"
        }));
    }
}
