//! GNSS capability: publishes `component::gnss::Sample` from the Webots `Gps`
//! device, whose reading is already a latitude/longitude/altitude triple.
//!
//! Webots models no fix quality, so the covariance is reported as all zeros:
//! the contract has no way to say "unknown", and any non-zero figure invented
//! here would describe an uncertainty nobody measured.

use anyhow::Result;
use phoxal::api;

use super::{SampledSpec, SensorStep, SimulatedSensor};

pub(crate) struct NativeGnss {
    gps: webots_rs::device::gps::Gps,
    spec: SampledSpec,
}

impl NativeGnss {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &SampledSpec) -> Result<Self> {
        let gps = webots.gps(spec.reference.to_string())?;
        gps.enable(spec.sampling_period_ms)?;
        Ok(Self {
            gps,
            spec: spec.clone(),
        })
    }
}

impl SimulatedSensor for NativeGnss {
    type Sample = api::component::gnss::Sample;

    fn schedule(&self) -> phoxal::SampleSchedule {
        self.spec.schedule
    }

    fn read(&mut self, _step: SensorStep) -> Result<Option<Self::Sample>> {
        let reading = self.gps.reading()?;
        Ok(Some(api::component::gnss::Sample {
            latitude: reading.position[0],
            longitude: reading.position[1],
            altitude: reading.position[2],
            position_covariance: [0.0; 9],
        }))
    }
}
