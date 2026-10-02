//! A component combining a derived standard encoder endpoint with a
//! component-specific calibration operation in one Runtime contract.

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
    use super::phoxal_runtime_fixture::Adapter;
    use phoxal::runtime::Runtime;

    #[test]
    fn standard_and_custom_endpoints_compose_in_one_contract() {
        use phoxal::runtime::outputs::OutputSet;
        let mut outputs: Vec<&str> = <Adapter as Runtime>::Outputs::FIELDS
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
            <Adapter as Runtime>::Outputs::FIELDS
                .iter()
                .any(|field| field.name == "encoder" && field.port_signature.is_some()),
            "the derived endpoint carries its typed signature"
        );
    }
}
