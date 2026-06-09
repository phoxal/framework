use std::collections::HashMap;

use crate::model::component::v1::CapabilityRef;
use crate::spatial::ray::Ray;
use crate::spatial::sensor::ResolvedSensorPose;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealth {
    Available,
    NoData,
    TimedOut,
    InvalidPayload,
    Disabled,
}

#[derive(Clone)]
pub struct SensorState {
    pose: ResolvedSensorPose,
    last_input_timestamp_ns: Option<u64>,
    health: SourceHealth,
    rays: Vec<Ray>,
}

impl SensorState {
    fn new(pose: ResolvedSensorPose) -> Self {
        Self {
            pose,
            last_input_timestamp_ns: None,
            health: SourceHealth::NoData,
            rays: Vec::new(),
        }
    }

    pub fn pose(&self) -> &ResolvedSensorPose {
        &self.pose
    }

    pub fn update(&mut self, timestamp_ns: u64, health: SourceHealth, rays: Vec<Ray>) {
        self.last_input_timestamp_ns = Some(timestamp_ns);
        self.health = health;
        self.rays = rays;
    }
}

pub struct SensorView<'a> {
    capability: &'a CapabilityRef,
    last_input_timestamp_ns: Option<u64>,
    health: SourceHealth,
    rays: &'a [Ray],
}

impl<'a> SensorView<'a> {
    pub const fn capability(&self) -> &'a CapabilityRef {
        self.capability
    }

    pub const fn last_input_timestamp_ns(&self) -> Option<u64> {
        self.last_input_timestamp_ns
    }

    pub const fn health(&self) -> SourceHealth {
        self.health
    }

    pub const fn rays(&self) -> &'a [Ray] {
        self.rays
    }
}

pub struct SensorStore {
    sensor_timeout_ns: u64,
    indices: HashMap<CapabilityRef, usize>,
    sensors: Vec<SensorState>,
}

impl SensorStore {
    pub fn new(sensors: &[ResolvedSensorPose], sensor_timeout_ns: u64) -> Self {
        Self {
            sensor_timeout_ns,
            indices: sensors
                .iter()
                .enumerate()
                .map(|(index, pose)| (pose.capability.clone(), index))
                .collect(),
            sensors: sensors.iter().cloned().map(SensorState::new).collect(),
        }
    }

    pub fn get_mut(&mut self, capability: &CapabilityRef) -> Option<&mut SensorState> {
        self.indices
            .get(capability)
            .copied()
            .and_then(|index| self.sensors.get_mut(index))
    }

    pub fn sensors(&self, now_ns: u64) -> impl Iterator<Item = SensorView<'_>> {
        self.sensors.iter().map(move |sensor| {
            let mut health = sensor.health;
            let mut rays = &[][..];
            if let Some(last_update_ns) = sensor.last_input_timestamp_ns {
                if now_ns.saturating_sub(last_update_ns) > self.sensor_timeout_ns {
                    health = SourceHealth::TimedOut;
                } else if health == SourceHealth::Available {
                    rays = sensor.rays.as_slice();
                }
            } else {
                health = SourceHealth::NoData;
            };

            SensorView {
                capability: &sensor.pose.capability,
                last_input_timestamp_ns: sensor.last_input_timestamp_ns,
                health,
                rays,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::model::component::v1::CapabilityRef;
    use crate::spatial::sensor::{ResolvedSensorKind, ResolvedSensorPose};
    use nalgebra::UnitQuaternion;

    use super::{SensorStore, SourceHealth};

    fn range_pose(capability_id: &str) -> ResolvedSensorPose {
        ResolvedSensorPose {
            capability: CapabilityRef::new("range", capability_id),
            offset_xyz_m: [0.0, 0.0, 0.0],
            yaw_rad: 0.0,
            local_rotation: UnitQuaternion::identity(),
            kind: ResolvedSensorKind::Range {
                field_of_view_rad: 0.0,
                max_range_m: 4.0,
            },
        }
    }

    #[test]
    fn registered_sensors_start_with_no_data() {
        let sensors = vec![range_pose("front")];
        let store = SensorStore::new(&sensors, 10);

        let views = store.sensors(0).collect::<Vec<_>>();

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].capability(), &sensors[0].capability);
        assert_eq!(views[0].last_input_timestamp_ns(), None);
        assert_eq!(views[0].health(), SourceHealth::NoData);
        assert!(views[0].rays().is_empty());
    }

    #[test]
    fn stale_available_sensor_reports_timeout_without_rays() {
        let sensors = vec![range_pose("front")];
        let mut store = SensorStore::new(&sensors, 10);
        let sensor = store
            .get_mut(&sensors[0].capability)
            .expect("registered sensor exists");
        sensor.update(
            10,
            SourceHealth::Available,
            vec![crate::spatial::ray::Ray::new(
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                4.0,
                0.05,
                1.0,
            )],
        );

        let views = store.sensors(21).collect::<Vec<_>>();

        assert_eq!(views[0].last_input_timestamp_ns(), Some(10));
        assert_eq!(views[0].health(), SourceHealth::TimedOut);
        assert!(views[0].rays().is_empty());
    }
}
