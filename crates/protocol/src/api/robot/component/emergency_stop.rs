/// Per-instance emergency-stop state.
#[derive(
    phoxal_macros::DescribeWire, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct State {
    pub engaged: bool,
}

phoxal_macros::protocol_fragment! {
    path robot / component(instance) / emergency_stop(capability);

    state: State<State>;
}
