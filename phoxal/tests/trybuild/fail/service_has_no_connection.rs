// The hardware connection belongs to a `robot.components` entry, and only a
// component driver is bound to one. A service can neither declare a connection
// kind nor read one.
use phoxal::prelude::*;

#[phoxal::service(id = "wired-service", connection = serial)]
struct WiredService;

#[phoxal::service(id = "reading-service")]
struct ReadingService;

impl Participant for ReadingService {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let _connection = ctx.connection()?;
        Ok(((), ()))
    }
}

fn main() {}
