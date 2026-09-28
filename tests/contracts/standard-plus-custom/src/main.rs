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

#[phoxal::runtime(contract = FixtureApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl phoxal::runtime::Runtime for Fixture {
    type Config = ();
    type State = ();

    fn init(&self, _ctx: &phoxal::runtime::InitContext, (): ()) -> phoxal::Result<()> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &phoxal::runtime::StepContext,
        state: (),
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<((), Self::Outputs)> {
        Ok((state, Self::Outputs::default()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Fixture)
}

#[cfg(test)]
mod tests {
    use super::Fixture;
    use phoxal::runtime::Runtime;

    #[test]
    fn standard_and_custom_endpoints_compose_in_one_contract() {
        use phoxal::runtime::outputs::OutputSet;
        let mut outputs: Vec<&str> = <Fixture as Runtime>::Outputs::FIELDS
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
            <Fixture as Runtime>::Outputs::FIELDS
                .iter()
                .any(|field| field.name == "encoder" && field.port_signature.is_some()),
            "the derived endpoint carries its typed signature"
        );
    }
}
