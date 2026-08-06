//! The hello-rover root brain: this project's one composition root.
//!
//! Robot-specific mission policy, intent selection, manual-override policy and
//! recovery are ordinary Rust code compiled into this binary. The example ships
//! the minimal no-op shape - `Config = ()`, `State = ()`, `Api = ()` - so the
//! project stays a pure authoring example.

use phoxal::prelude::*;

#[phoxal::brain]
struct Brain;

impl Participant for Brain {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Brain>()
}
