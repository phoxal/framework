//! `localize` - the official odometry-anchored localization participant.
//!
//! A scheduled participant that subscribes to `odometry/state` and republishes
//! `localize/state`, the localization estimate consumed by downstream runtimes
//! such as `map`, `explore`, and `follow`.
//! The estimate is transparent: map-frame and odometry-frame are the same
//! frame, so the odometry pose is the map-frame pose, and confidence decays
//! linearly to zero as the latest odometry sample ages past the staleness
//! window.
//! Until the first odometry fix arrives it publishes nothing, so downstream
//! runtimes never mistake a fabricated origin pose for a real estimate.

use std::time::Duration;

use phoxal::api;
use phoxal::prelude::*;

const LOCALIZE_STALE: Duration = Duration::from_secs(1);

pub(crate) struct Api {
    odometry: StateView<api::odometry::State>,
    state: StatePublisher<api::localize::LocalizationState>,
}

pub(crate) struct LocalizeState {
    // Runtime-private typed state (not handles).
    last_odometry: Option<Timed<api::odometry::State>>,
}

#[phoxal::service(state = LocalizeState, api = Api)]
pub(crate) struct Localize;

impl Participant for Localize {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let odometry = ctx
            .state_view(api::topics().odometry().state().client())
            .await?;
        let state = ctx.state_publisher(api::topics().localize().state().owner())?;

        Ok((
            LocalizeState {
                last_odometry: None,
            },
            Api { odometry, state },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.last_odometry = None;
        Ok(())
    }

    #[phoxal::step(hz = 20)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        if let Some(observed) = api.odometry.observed()
            && let Some(at) = observed.metadata.produced_exactly_at()
        {
            state.last_odometry = Some(Timed::new(observed.body.clone(), at));
        }

        // Publish only from a real, finite, fresh odometry sample, so consumers
        // never see a fabricated origin. Each gate hands the next one the value
        // it proved, rather than re-deriving the proof.
        let Some(odometry) = state.last_odometry.as_ref() else {
            return Ok(());
        };
        let now = step.now();
        // A sample from a replaced world, from this step's future, or older than
        // the window is not usable; `fresh_within` answers all three and fails
        // closed across timelines.
        if !odometry.fresh_within(now, LOCALIZE_STALE) {
            return Ok(());
        }
        if !odometry_is_finite(&odometry.body) {
            anyhow::bail!("odometry sample contains a non-finite value");
        }

        // Freshness already established that `at` is on this step's timeline and
        // at or before `now`, so this subtraction cannot fail.
        let age = now.duration_since(odometry.at)?;
        let confidence = confidence_for(age);
        api.state
            .publish(&step.token, localization_from(&odometry.body, confidence))?;
        Ok(())
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

    use super::{Duration, LOCALIZE_STALE, confidence_for, localization_from, odometry_is_finite};

    fn state(x_m: f64) -> api::odometry::State {
        api::odometry::State {
            x_m,
            y_m: 0.0,
            yaw_rad: 0.0,
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        }
    }

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

    /// Every field is checked, so a non-finite value anywhere in the sample
    /// stops the estimate rather than propagating into a published pose.
    #[test]
    fn a_non_finite_field_anywhere_makes_the_sample_unusable() {
        assert!(odometry_is_finite(&state(0.0)));

        for broken in [
            api::odometry::State {
                x_m: f64::NAN,
                ..state(0.0)
            },
            api::odometry::State {
                y_m: f64::INFINITY,
                ..state(0.0)
            },
            api::odometry::State {
                yaw_rad: f64::NEG_INFINITY,
                ..state(0.0)
            },
            api::odometry::State {
                linear_x_mps: f32::NAN,
                ..state(0.0)
            },
            api::odometry::State {
                angular_z_radps: f32::INFINITY,
                ..state(0.0)
            },
        ] {
            assert!(
                !odometry_is_finite(&broken),
                "{broken:?} must not be finite"
            );
        }
    }
}
