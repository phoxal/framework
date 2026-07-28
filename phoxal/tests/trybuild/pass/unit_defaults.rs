use phoxal::prelude::*;

#[phoxal::service(id = "unit-defaults")]
struct UnitDefaults;

impl Participant for UnitDefaults {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() {
    fn accepts<T: Participant<Config = (), State = (), Api = ()>>() {}
    accepts::<UnitDefaults>();
}
