//! GNSS capability: publishes `component::gnss::Sample` from the Webots
//! `Gps` device. Moved from the monolith's `GnssSpec` (main.rs:600-604) and
//! `NativeGnss` (main.rs:1465-1496).

use anyhow::{Result, anyhow};
use phoxal::model::component::v1::capability::GnssCoordinateSystem;
use phoxal_api::y2026_1 as api;

use super::{SampledSpec, is_due};

#[derive(Clone, Debug)]
pub(crate) struct GnssSpec {
    pub(crate) sampled: SampledSpec,
    pub(crate) coordinate_system: GnssCoordinateSystem,
}

pub(crate) struct NativeGnss {
    gps: webots_rs::device::gps::Gps,
    spec: GnssSpec,
}

impl NativeGnss {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &GnssSpec) -> Result<Self> {
        let gps = webots
            .gps(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        gps.enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            gps,
            spec: spec.clone(),
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::gnss::Sample>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let reading = self.gps.reading().map_err(|error| anyhow!(error))?;
        let _coordinate_system = self.spec.coordinate_system;
        Ok(Some(api::component::gnss::Sample {
            latitude: reading.position[0],
            longitude: reading.position[1],
            altitude: reading.position[2],
            position_covariance: [0.0; 9],
        }))
    }
}
