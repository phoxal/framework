use anyhow::Result;
use phoxal::api::component::capability::gnss::v1 as gnss;
use phoxal::api::frame::v1::FrameId;
use phoxal::api::localize::v1::{
    LocalizationMode, LocalizationSource, LocalizationStatus, LocalizationStatusReason,
    PoseEstimate,
};
use phoxal::api::odometry::v1::OdometryEstimate;
use phoxal::bus::typed::Received;
use phoxal::model::component::v1::capability::GnssCoordinateSystem;
use phoxal::runtime::clock::Step;

use crate::geodetic::geodetic_to_enu;
use crate::runtime::{BackendUpdate, LocalizeBackend, initial_sensor_integration_revision};

const MAP_FRAME_ID: &str = "map";
const BASE_FRAME_ID: &str = "base_footprint";
const IDENTITY_ROTATION_XYZW: [f64; 4] = [0.0, 0.0, 0.0, 1.0];

pub(crate) struct GnssAnchoredBackend {
    coordinate_system: GnssCoordinateSystem,
    wgs84_origin: Option<Wgs84Origin>,
    latest_gnss: Option<Received<gnss::Sample>>,
    latest_odometry: Option<Received<OdometryEstimate>>,
    initial_revision_emitted: bool,
}

impl GnssAnchoredBackend {
    pub(crate) const fn new(coordinate_system: GnssCoordinateSystem) -> Self {
        Self {
            coordinate_system,
            wgs84_origin: None,
            latest_gnss: None,
            latest_odometry: None,
            initial_revision_emitted: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Wgs84Origin {
    lat_deg: f64,
    lon_deg: f64,
    alt_m: f64,
}

impl Wgs84Origin {
    fn from_sample(sample: &gnss::Sample) -> Self {
        Self {
            lat_deg: sample.latitude(),
            lon_deg: sample.longitude(),
            alt_m: sample.altitude(),
        }
    }
}

#[async_trait::async_trait]
impl LocalizeBackend for GnssAnchoredBackend {
    fn name(&self) -> LocalizationSource {
        LocalizationSource::GnssAnchored
    }

    fn ingest_odometry(&mut self, sample: Received<OdometryEstimate>) {
        self.latest_odometry = Some(sample);
    }

    fn ingest_gnss(&mut self, sample: Received<gnss::Sample>) {
        if self.coordinate_system == GnssCoordinateSystem::Wgs84 && self.wgs84_origin.is_none() {
            self.wgs84_origin = Some(Wgs84Origin::from_sample(&sample.value));
        }
        self.latest_gnss = Some(sample);
    }

    fn step(&mut self, step: Step) -> Result<BackendUpdate> {
        let Some(sample) = &self.latest_gnss else {
            return Ok(BackendUpdate {
                mode: LocalizationMode::Initializing,
                pose: None,
                velocity: None,
                covariance: None,
                imu_bias: None,
                status: LocalizationStatus {
                    healthy: false,
                    reasons: vec![
                        LocalizationStatusReason::SensorMissing,
                        LocalizationStatusReason::BackendInitializing,
                    ],
                },
                valid_at_ns: None,
                new_revision: None,
                keyframe: None,
            });
        };

        let rotation_xyzw = self
            .latest_odometry
            .as_ref()
            .map(|odometry| odometry.value.pose.rotation_xyzw)
            .unwrap_or(IDENTITY_ROTATION_XYZW);
        let pose = PoseEstimate {
            frame_id: FrameId::new(MAP_FRAME_ID),
            child_frame_id: FrameId::new(BASE_FRAME_ID),
            translation_m: self.translation_m(&sample.value),
            rotation_xyzw,
        };
        let new_revision = initial_sensor_integration_revision(
            LocalizationMode::Tracking,
            self.initial_revision_emitted,
        );
        if new_revision.is_some() {
            self.initial_revision_emitted = true;
        }

        Ok(BackendUpdate {
            mode: LocalizationMode::Tracking,
            pose: Some(pose),
            keyframe: None,
            velocity: None,
            covariance: None,
            imu_bias: None,
            status: LocalizationStatus {
                healthy: true,
                reasons: Vec::new(),
            },
            valid_at_ns: Some(sample.at_ns.unwrap_or_else(|| step.tick.time_ns())),
            new_revision,
        })
    }
}

impl GnssAnchoredBackend {
    fn translation_m(&self, sample: &gnss::Sample) -> [f64; 3] {
        match self.coordinate_system {
            GnssCoordinateSystem::Local => {
                [sample.latitude(), sample.longitude(), sample.altitude()]
            }
            GnssCoordinateSystem::Wgs84 => {
                let Some(origin) = self.wgs84_origin else {
                    return [0.0, 0.0, 0.0];
                };
                geodetic_to_enu(
                    sample.latitude(),
                    sample.longitude(),
                    sample.altitude(),
                    origin.lat_deg,
                    origin.lon_deg,
                    origin.alt_m,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::localize::v1::{AffectedKeyframeSummary, LocalizationRevisionCause};
    use phoxal::api::odometry::v1::{
        Covariance as OdometryCovariance, PoseEstimate as OdometryPoseEstimate, Status, StatusMode,
        VelocityEstimate as OdometryVelocityEstimate,
    };
    use phoxal::api::simulation::clock::v1::Clock;

    use super::*;

    #[test]
    fn initializing_until_first_gnss_sample() {
        let mut backend = local_backend();

        let update = step_backend(&mut backend, step_at(20_000_000));

        assert_eq!(update.mode, LocalizationMode::Initializing);
        assert_eq!(update.pose, None);
        assert_eq!(update.new_revision, None);
    }

    #[test]
    fn tracking_pose_uses_gnss_translation_and_odometry_rotation() {
        let mut backend = local_backend();
        let odometry_rotation = [0.0, 0.0, 0.382_683_432_4, 0.923_879_532_5];

        backend.ingest_odometry(odometry_sample(10, odometry_rotation));
        backend.ingest_gnss(gnss_sample(11, 1.25, -2.5, 0.75));
        let update = step_backend(&mut backend, step_at(20_000_000));

        assert_eq!(update.mode, LocalizationMode::Tracking);
        assert_eq!(update.valid_at_ns, Some(11));
        assert_eq!(
            update.pose.as_ref().map(|pose| pose.translation_m),
            Some([1.25, -2.5, 0.75])
        );
        assert_eq!(
            update.pose.as_ref().map(|pose| pose.rotation_xyzw),
            Some(odometry_rotation)
        );
        assert_eq!(
            update
                .pose
                .as_ref()
                .map(|pose| (&pose.frame_id, &pose.child_frame_id)),
            Some((&FrameId::new(MAP_FRAME_ID), &FrameId::new(BASE_FRAME_ID)))
        );
        assert!(update.status.healthy);
        assert_eq!(update.status.reasons, Vec::new());
        assert_eq!(update.keyframe, None);
    }

    #[test]
    fn tracking_pose_uses_identity_rotation_before_odometry() {
        let mut backend = local_backend();

        backend.ingest_gnss(gnss_sample(11, 1.25, -2.5, 0.75));
        let update = step_backend(&mut backend, step_at(20_000_000));

        assert_eq!(
            update.pose.as_ref().map(|pose| pose.rotation_xyzw),
            Some(IDENTITY_ROTATION_XYZW)
        );
    }

    #[test]
    fn emits_initial_revision_once() {
        let mut backend = local_backend();
        backend.ingest_gnss(gnss_sample(11, 1.25, -2.5, 0.75));

        let first = step_backend(&mut backend, step_at(20_000_000));
        let second = step_backend(&mut backend, step_at(40_000_000));

        assert_eq!(
            first.new_revision,
            Some(crate::runtime::NewRevision {
                cause: LocalizationRevisionCause::SensorIntegration,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            })
        );
        assert_eq!(second.new_revision, None);
    }

    #[test]
    fn local_coordinate_system_keeps_passthrough_translation() {
        let mut backend = local_backend();

        backend.ingest_gnss(gnss_sample(11, 52.225_611, 6.883_363_5, 12.0));
        let update = step_backend(&mut backend, step_at(20_000_000));

        assert_eq!(
            update.pose.as_ref().map(|pose| pose.translation_m),
            Some([52.225_611, 6.883_363_5, 12.0])
        );
    }

    #[test]
    fn wgs84_coordinate_system_anchors_first_fix_and_converts_to_enu() {
        let mut backend = GnssAnchoredBackend::new(GnssCoordinateSystem::Wgs84);

        backend.ingest_gnss(gnss_sample(11, 52.225_611, 6.883_363_5, 12.0));
        let origin = step_backend(&mut backend, step_at(20_000_000));
        backend.ingest_gnss(gnss_sample(12, 52.225_701, 6.883_510, 14.5));
        let moved = step_backend(&mut backend, step_at(40_000_000));

        assert_translation_close(
            origin.pose.as_ref().map(|pose| pose.translation_m),
            [0.0, 0.0, 0.0],
            1.0e-6,
        );
        assert_translation_close(
            moved.pose.as_ref().map(|pose| pose.translation_m),
            [10.0, 10.0, 2.5],
            0.15,
        );
    }

    fn local_backend() -> GnssAnchoredBackend {
        GnssAnchoredBackend::new(GnssCoordinateSystem::Local)
    }

    fn step_at(time_ns: u64) -> Step {
        Step::new(Clock::new(1, time_ns / 20_000_000, time_ns, 20_000_000))
    }

    fn step_backend(backend: &mut GnssAnchoredBackend, step: Step) -> BackendUpdate {
        match backend.step(step) {
            Ok(update) => update,
            Err(error) => panic!("GNSS-anchored step failed: {error:#}"),
        }
    }

    fn gnss_sample(
        timestamp_ns: u64,
        latitude: f64,
        longitude: f64,
        altitude: f64,
    ) -> Received<gnss::Sample> {
        Received {
            at_ns: Some(timestamp_ns),
            value: gnss::Sample::new(latitude, longitude, altitude, [0.0; 9]),
        }
    }

    fn odometry_sample(timestamp_ns: u64, rotation_xyzw: [f64; 4]) -> Received<OdometryEstimate> {
        Received {
            at_ns: Some(timestamp_ns),
            value: OdometryEstimate {
                pose: OdometryPoseEstimate {
                    frame_id: FrameId::new("odom"),
                    child_frame_id: FrameId::new("base_footprint"),
                    translation_m: [0.0, 0.0, 0.0],
                    rotation_xyzw,
                },
                velocity: OdometryVelocityEstimate {
                    frame_id: FrameId::new("base_footprint"),
                    linear_mps: [0.0, 0.0, 0.0],
                    angular_radps: [0.0, 0.0, 0.0],
                },
                covariance: Some(OdometryCovariance {
                    values: vec![0.0; 36],
                }),
                status: Status {
                    mode: StatusMode::Tracking,
                    reasons: Vec::new(),
                },
            },
        }
    }

    fn assert_translation_close(actual: Option<[f64; 3]>, expected: [f64; 3], tolerance: f64) {
        let Some(actual) = actual else {
            panic!("expected pose translation");
        };
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= tolerance,
                "expected {actual} to be within {tolerance} of {expected}"
            );
        }
    }
}
