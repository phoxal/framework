//! `localize` - the official odometry-anchored localization participant.
//!
//! A scheduled participant that subscribes to `odometry/state` and republishes
//! `localize/state`, the localization estimate consumed by downstream runtimes
//! such as `map`, `explore`, and `follow`.
//! This first version is transparent: the odometry pose is treated directly as
//! the map-frame pose, and confidence decays linearly to zero as the latest
//! odometry sample ages past the staleness window.
//! Until the first odometry fix arrives it publishes nothing, so downstream
//! runtimes never mistake a fabricated origin pose for a real estimate.
//! It does not implement ORB-SLAM3, visual-inertial localization, or GNSS
//! anchoring; map-frame and odometry-frame are assumed identical.

use phoxal::api;
use phoxal::prelude::*;

const LOCALIZE_STALE_NS: u64 = 1_000_000_000; // 1 s

#[derive(phoxal::Api)]
struct Api {
    odometry: Subscriber<api::odometry::State>,
    state: Publisher<api::localize::LocalizationState>,
}

#[phoxal::service(id = "localize", config = ())]
struct Localize {
    // Runtime-private typed state (not handles).
    last_odometry: Option<(api::odometry::State, LogicalTime)>,
}

#[phoxal::behavior]
impl Localize {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let odometry = ctx
            .subscriber(api::topic::new().odometry().state(), 32)
            .await?;
        let state = ctx
            .publisher(api::topic::internal::new(cap).localize().state())
            .await?;

        Ok((
            Self {
                last_odometry: None,
            },
            Self::Api { odometry, state },
        ))
    }

    #[step(hz = 20)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        while let Some(received) = api.odometry.try_recv() {
            self.last_odometry = Some((
                received.body,
                LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
            ));
        }

        let now = step.time();
        if !odometry_is_usable(self.last_odometry.as_ref(), now)? {
            return Ok(());
        }

        // The input gate proves a real, finite, fresh odometry sample exists;
        // publish nothing otherwise so consumers never see a fabricated origin.
        let (odometry, produced_at) = self
            .last_odometry
            .as_ref()
            .expect("usable odometry requires a sample");

        let confidence = confidence_for(now.time_ns().saturating_sub(produced_at.time_ns()));
        api.state
            .publish_at(step.time(), localization_from(odometry, confidence))
            .await?;
        Ok(())
    }
}

fn odometry_is_usable(
    sample: Option<&(api::odometry::State, LogicalTime)>,
    now: LogicalTime,
) -> Result<bool> {
    match sample {
        None => Ok(false),
        Some((_, produced_at)) if produced_at.epoch() != now.epoch() => Ok(false),
        Some((_, produced_at)) if produced_at.time_ns() > now.time_ns() => Ok(false),
        Some((_, produced_at))
            if now.time_ns().saturating_sub(produced_at.time_ns()) > LOCALIZE_STALE_NS =>
        {
            Ok(false)
        }
        Some((odometry, _)) if !odometry_is_finite(odometry) => {
            anyhow::bail!("odometry sample contains a non-finite value")
        }
        Some(_) => Ok(true),
    }
}

fn odometry_is_finite(state: &api::odometry::State) -> bool {
    state.x_m.is_finite()
        && state.y_m.is_finite()
        && state.yaw_rad.is_finite()
        && state.linear_x_mps.is_finite()
        && state.angular_z_radps.is_finite()
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
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::bus::LogicalTime;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    use super::{
        LOCALIZE_STALE_NS, Localize, confidence_for, localization_from, odometry_is_usable,
    };

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
    fn odometry_gate_rejects_unavailable_stale_future_and_invalid_samples() {
        let state = |x_m| api::odometry::State {
            x_m,
            y_m: 0.0,
            yaw_rad: 0.0,
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        };
        let now = LogicalTime::new(2, LOCALIZE_STALE_NS + 10);
        assert!(!odometry_is_usable(None, now).unwrap());
        assert!(odometry_is_usable(Some(&(state(0.0), LogicalTime::new(2, 10))), now).unwrap());
        assert!(!odometry_is_usable(Some(&(state(0.0), LogicalTime::new(2, 9))), now).unwrap());
        assert!(
            !odometry_is_usable(
                Some(&(state(0.0), LogicalTime::new(2, now.time_ns() + 1))),
                now
            )
            .unwrap()
        );
        assert!(
            !odometry_is_usable(Some(&(state(0.0), LogicalTime::new(1, now.time_ns()))), now)
                .unwrap()
        );
        assert!(
            odometry_is_usable(
                Some(&(state(f64::NAN), LogicalTime::new(2, now.time_ns()))),
                now
            )
            .is_err()
        );
    }

    #[test]
    fn api_reports_contracts() {
        assert_eq!(<Localize as Participant>::ID, "localize");

        let contracts = <<Localize as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::odometry::State as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::localize::LocalizationState as ContractBody>::TOPIC
                && c.role == ContractRole::Publish
        }));
    }
}
