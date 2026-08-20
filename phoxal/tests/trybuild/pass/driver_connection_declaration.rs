// The declared connection kind is the driver's own: it decides what
// `ctx.connection()` yields. A driver that declares `serial` reads a `Serial`
// payload with no variant to match, and one that declares nothing gets the
// whole vocabulary and decides for itself.
use phoxal::model::connection::{Connection, Serial};
use phoxal::prelude::*;

#[phoxal::driver(id = "any-connection", state = Option<Serial>)]
struct AnyConnection;

impl Participant for AnyConnection {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let connection: Connection = ctx.connection()?;
        Ok((
            match connection {
                Connection::Serial(serial) => Some(serial),
                _ => None,
            },
            (),
        ))
    }
}

#[phoxal::driver(id = "serial-only", connection = serial, state = Serial)]
struct SerialOnly;

impl Participant for SerialOnly {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let serial: Serial = ctx.connection()?;
        let _ = (serial.port.as_str(), serial.baud);
        Ok((serial, ()))
    }
}

fn main() {}
