/// Per-instance emergency-stop state.
#[derive(Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub engaged: bool,
}

phoxal_macros::phoxal_api_fragment! {
    path component(instance) / emergency_stop(capability);

    version v0_1;

    topic state: State<State>;
}
