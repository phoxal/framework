/// Per-instance emergency-stop state.
#[derive(
    phoxal_macros::DescribeWire, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct State {
    pub engaged: bool,
}
