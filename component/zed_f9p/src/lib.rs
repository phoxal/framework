//! `zed_f9p` - u-blox ZED-F9P GNSS component driver stub.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::capability::Capability;
use phoxal::prelude::*;

const STEP_HZ: f64 = 10.0;

#[derive(phoxal::Api)]
pub struct Api {
    gnss: Vec<Publisher<api::component::gnss::Sample>>,
}

#[phoxal::driver(id = "zed_f9p", config = ())]
pub struct ZedF9p {
    gnss_divisors: Vec<u64>,
}

#[derive(Debug, Clone)]
struct GnssSlot {
    capability_id: String,
    divisor: u64,
}

#[phoxal::behavior]
impl ZedF9p {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the owner
        // (`internal`) topic builder requires. This driver OWNS its component node.
        let cap = ctx.owner_capability();
        let instance = ctx.component()?.to_string();
        let slots = {
            let robot = ctx.robot()?;
            let spec = robot.component_for_instance(&instance)?;
            let mut slots = Vec::new();

            for (capability_id, capability) in &spec.capabilities {
                if let Capability::Gnss(config) = capability {
                    validate_publish_rate(capability_id, config.publish_rate_hz)?;
                    let _coordinate_system = config.coordinate_system;
                    slots.push(GnssSlot {
                        capability_id: capability_id.to_string(),
                        divisor: divisor_for_rate(STEP_HZ, config.publish_rate_hz),
                    });
                }
            }

            slots
        };

        if slots.is_empty() {
            bail!("zed_f9p requires at least one gnss capability");
        }

        let mut gnss = Vec::new();
        let mut gnss_divisors = Vec::new();
        for slot in slots {
            gnss.push(
                ctx.publisher(
                    api::topic::internal::new(cap)
                        .component(&instance)
                        .gnss(&slot.capability_id)
                        .sample(),
                )
                .await?,
            );
            gnss_divisors.push(slot.divisor);
        }

        Ok((Self { gnss_divisors }, Self::Api { gnss }))
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let at = step.time();
        let step_index = step.step_index();

        for (publisher, divisor) in api.gnss.iter().zip(&self.gnss_divisors) {
            if is_due(step_index, *divisor) {
                publisher.publish_at(at, gnss_sample()).await?;
            }
        }

        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        Ok(())
    }
}

fn validate_publish_rate(capability_id: &str, publish_rate_hz: f64) -> Result<()> {
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        bail!("capability '{capability_id}' publish_rate_hz must be > 0");
    }
    Ok(())
}

fn divisor_for_rate(step_hz: f64, publish_rate_hz: f64) -> u64 {
    (step_hz / publish_rate_hz).round().max(1.0) as u64
}

fn is_due(step_index: u64, divisor: u64) -> bool {
    divisor <= 1 || step_index % divisor == 0
}

fn gnss_sample() -> api::component::gnss::Sample {
    api::component::gnss::Sample {
        latitude: 0.0,
        longitude: 0.0,
        altitude: 0.0,
        position_covariance: [0.0; 9],
    }
}

#[cfg(test)]
mod tests {
    use super::{ZedF9p, divisor_for_rate, gnss_sample};
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    #[test]
    fn divisor_and_stub_sample_are_stable() {
        assert_eq!(divisor_for_rate(10.0, 10.0), 1);
        assert_eq!(divisor_for_rate(10.0, 5.0), 2);

        let sample = gnss_sample();
        assert_eq!(sample.latitude, 0.0);
        assert_eq!(sample.position_covariance, [0.0; 9]);
    }

    #[test]
    fn api_reports_per_component_contracts() {
        assert_eq!(<ZedF9p as Participant>::ID, "zed_f9p");

        let contracts = <<ZedF9p as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(
            contracts.iter().any(|c| {
                c.topic == <api::component::gnss::Sample as ContractBody>::TOPIC
                    && c.role == ContractRole::Publish
            }),
            "expected a Publish contract for {} in {contracts:?}",
            <api::component::gnss::Sample as ContractBody>::TOPIC
        );
    }
}
