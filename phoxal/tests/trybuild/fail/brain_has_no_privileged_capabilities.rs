// The brain owns policy, not privilege: it is not component-bound, which is a
// driver capability.
use phoxal::prelude::*;

#[phoxal::brain]
struct PrivilegedBrain;

impl Participant for PrivilegedBrain {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _component = ctx.component()?;
        Ok(((), ()))
    }
}

fn main() {}
