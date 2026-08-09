/// A per-LED on/off command.
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    On,
    Off,
}

phoxal_macros::phoxal_api_fragment! {
    path component(instance) / led(capability);

    version v0_1;

    command command: Setpoint<Command>;
}
