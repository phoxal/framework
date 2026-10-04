struct Service;

#[phoxal::runtime(period_ms = 20)]
impl phoxal::runtime::Runtime for Service {
    type Config = ();
    type State = ();
    type Inputs = ();
    type Outputs = ();

    fn init(&self, _: &phoxal::runtime::InitContext, _: ()) -> phoxal::Result<()> {
        Ok(())
    }

    fn step(&self, _: &phoxal::runtime::StepContext, _: (), _: &()) -> phoxal::Result<((), ())> {
        Ok(((), ()))
    }
}

fn main() {}
