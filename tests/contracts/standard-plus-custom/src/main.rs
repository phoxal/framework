//! A component combining a derived standard encoder endpoint with a
//! component-specific calibration operation in one Runtime contract.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

phoxal::api!();

use phoxal::contracts::{Empty, RequestReply};

/// The component-specific endpoint surface: the derived standard encoder
/// endpoint is spliced in beside it.
#[phoxal::endpoints]
pub struct FixtureApi {
    #[phoxal::operation(
        contract = "fixture.calibrate.v1.Calibrate",
        max_items = 4,
        max_bytes = 1024
    )]
    calibrate: RequestReply<Empty, Empty>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Fixture;

#[phoxal::runtime(contract = FixtureApi, period_ms = 20)]
impl Fixture {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self)
    }

    #[handle(calibrate)]
    fn calibrate(
        &mut self,
        _ctx: &mut phoxal::runtime::Context<'_, Self>,
        _request: Empty,
    ) -> phoxal::Result<Empty> {
        Ok(Empty {})
    }

    #[step]
    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<Fixture>()
}

#[cfg(test)]
mod tests {
    use phoxal::runtime::RuntimeContract;
    type Outputs = <super::FixtureApi as RuntimeContract>::Outputs;

    #[test]
    fn standard_and_custom_endpoints_compose_in_one_contract() {
        let mut outputs: Vec<&str> = <Outputs as phoxal::runtime::outputs::OutputSet>::FIELDS
            .iter()
            .map(|field| field.name)
            .collect();
        outputs.sort_unstable();
        assert_eq!(
            outputs,
            ["calibrate_replies", "encoder"],
            "the derived standard output and the component-specific operation compose"
        );
        assert!(
            <Outputs as phoxal::runtime::outputs::OutputSet>::FIELDS
                .iter()
                .any(|field| field.name == "encoder" && field.port_signature.is_some()),
            "the derived endpoint carries its typed signature"
        );
    }
}
