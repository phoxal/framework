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

use std::time::Duration;

use phoxal::api;
use phoxal::prelude::*;

const LOCALIZE_STALE: Duration = Duration::from_secs(1);

pub struct Api {
    odometry: Subscriber<api::odometry::State>,
    state: StatePublisher<api::localize::LocalizationState>,
}

pub struct LocalizeState {
    // Runtime-private typed state (not handles).
    last_odometry: Option<(api::odometry::State, RobotInstant)>,
}

#[phoxal::service(state = LocalizeState, api = Api)]
pub struct Localize;

impl Participant for Localize {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let odometry = ctx
            .subscriber(api::topic::client().odometry().state(), 32)
            .await?;
        let state = ctx
            .state_publisher(api::topic::owner().localize().state())
            .await?;

        Ok((
            LocalizeState {
                last_odometry: None,
            },
            Api { odometry, state },
        ))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        state.last_odometry = None;
        Ok(())
    }

    #[phoxal::step(hz = 20)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        while let Some(observed) = api.odometry.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                state.last_odometry = Some((observed.body, at));
            }
        }

        let now = step.now();
        if !odometry_is_usable(state.last_odometry.as_ref(), now)? {
            return Ok(());
        }

        // The input gate proves a real, finite, fresh odometry sample exists;
        // publish nothing otherwise so consumers never see a fabricated origin.
        let (odometry, produced_at) = state
            .last_odometry
            .as_ref()
            .expect("usable odometry requires a sample");

        let age = now
            .duration_since(*produced_at)
            .expect("a usable sample is on this step's timeline");
        let confidence = confidence_for(age);
        api.state
            .publish(step.token(), localization_from(odometry, confidence))?;
        Ok(())
    }
}

fn odometry_is_usable(
    sample: Option<&(api::odometry::State, RobotInstant)>,
    now: RobotInstant,
) -> Result<bool> {
    match sample {
        None => Ok(false),
        // A sample from a replaced world, from this step's future, or older
        // than the window is not usable. The checked predicate answers all
        // three, and a cross-timeline comparison can never silently pass.
        Some((_, produced_at))
            if !TimeWindow::exact(*produced_at)
                .possibly_fresh_within(now, LOCALIZE_STALE)
                .unwrap_or(false) =>
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

fn confidence_for(age: Duration) -> f32 {
    let age_fraction = age.as_secs_f64() / LOCALIZE_STALE.as_secs_f64();
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

#[cfg(test)]
mod tests {
    use phoxal::api;
    use phoxal::bus::{RobotInstant, TimelineId};

    use super::{Duration, LOCALIZE_STALE, confidence_for, localization_from, odometry_is_usable};

    #[test]
    fn confidence_decays_with_age() {
        assert_eq!(confidence_for(Duration::ZERO), 1.0);
        assert_eq!(confidence_for(LOCALIZE_STALE), 0.0);
        assert!((confidence_for(LOCALIZE_STALE / 2) - 0.5).abs() < 1e-6);
        assert_eq!(
            confidence_for(LOCALIZE_STALE + Duration::from_nanos(1)),
            0.0
        );
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
        let line = TimelineId::mint();
        let replaced = TimelineId::mint();
        let stale_ns = u64::try_from(LOCALIZE_STALE.as_nanos()).unwrap();
        let now = RobotInstant::new(line, stale_ns + 10);
        let at = |ticks| RobotInstant::new(line, ticks);

        assert!(!odometry_is_usable(None, now).unwrap());
        assert!(odometry_is_usable(Some(&(state(0.0), at(10))), now).unwrap());
        assert!(
            !odometry_is_usable(Some(&(state(0.0), at(9))), now).unwrap(),
            "a sample older than the window is not usable"
        );
        assert!(
            !odometry_is_usable(Some(&(state(0.0), at(now.ticks() + 1))), now).unwrap(),
            "a sample from this step's future is not usable"
        );
        assert!(
            !odometry_is_usable(
                Some(&(state(0.0), RobotInstant::new(replaced, now.ticks()))),
                now
            )
            .unwrap(),
            "a sample from a replaced world is incomparable, never usable"
        );
        assert!(odometry_is_usable(Some(&(state(f64::NAN), at(now.ticks()))), now).is_err());
    }
}
