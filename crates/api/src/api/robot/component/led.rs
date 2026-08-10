/// A per-LED on/off command.
#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum Command {
    On,
    Off,
}

phoxal_macros::phoxal_api_fragment! {
    path robot / component(instance) / led(capability);

    command command: Setpoint<Command>;
}
