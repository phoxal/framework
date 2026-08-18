/// One audio frame as raw encoded bytes.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Frame {
    pub data: Vec<u8>,
}
