//! The conveniences declared beside the contracts in [`crate::behavior`].

use std::time::Duration;

use crate::v0_2 as api;

#[test]
fn stopped_target_commands_no_motion() {
    let stopped = api::drive::Target::stopped();
    assert_eq!(stopped.linear_x_mps(), 0.0);
    assert_eq!(stopped.angular_z_radps(), 0.0);
}

#[test]
fn target_constructor_rejects_non_finite_components() {
    assert!(api::drive::Target::try_new(f32::NAN, 0.0).is_err());
    assert!(api::drive::Target::try_new(0.0, f32::INFINITY).is_err());
}

#[test]
fn a_finite_positively_confident_estimate_is_usable() {
    assert!(
        api::localize::LocalizationState {
            x_m: 1.0,
            y_m: -2.0,
            yaw_rad: 0.5,
            confidence: 0.01,
        }
        .is_usable()
    );
}

/// Zero confidence is how a live localizer says it has not converged. It is the
/// clause that keeps a well-formed publication from being read as a fix.
#[test]
fn a_zero_confidence_estimate_is_not_usable() {
    assert!(
        !api::localize::LocalizationState {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: 0.5,
            confidence: 0.0,
        }
        .is_usable()
    );
}

#[test]
fn a_non_finite_component_makes_an_estimate_unusable() {
    let usable = api::localize::LocalizationState {
        x_m: 1.0,
        y_m: 2.0,
        yaw_rad: 0.5,
        confidence: 0.9,
    };

    for broken in [
        api::localize::LocalizationState {
            x_m: f64::NAN,
            ..usable.clone()
        },
        api::localize::LocalizationState {
            y_m: f64::INFINITY,
            ..usable.clone()
        },
        api::localize::LocalizationState {
            yaw_rad: f64::NEG_INFINITY,
            ..usable.clone()
        },
        api::localize::LocalizationState {
            confidence: f32::NAN,
            ..usable.clone()
        },
    ] {
        assert!(!broken.is_usable(), "{broken:?} must not be usable");
    }
}

fn request_id(value: &str) -> api::navigation::RequestId {
    api::navigation::RequestId {
        value: value.to_string(),
    }
}

#[test]
fn a_bounded_ascii_token_is_a_valid_request_id() {
    for value in ["a", "run-7", "mission_2.step3", &"x".repeat(128)] {
        assert!(request_id(value).is_valid(), "{value:?} must be valid");
    }
}

#[test]
fn empty_oversized_and_non_token_request_ids_are_refused() {
    for value in [
        "",
        "   ",
        &"x".repeat(129),
        "has space",
        "sla/sh",
        "new\nline",
    ] {
        assert!(!request_id(value).is_valid(), "{value:?} must be refused");
    }
}

/// The identity is the key, not a copy of the string inside it. `Ord` is what
/// lets a consumer track requests in an ordered map without unwrapping the
/// newtype, so it is pinned here rather than left to a downstream derive.
#[test]
fn request_ids_order_by_their_value() {
    let mut ids = [request_id("c"), request_id("a"), request_id("b")];
    ids.sort();
    assert_eq!(
        ids.map(|id| id.value),
        ["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

#[test]
fn the_encoder_staleness_horizon_is_the_published_one() {
    assert_eq!(
        api::component::encoder::Sample::STALE_AFTER,
        Duration::from_millis(200)
    );
}
