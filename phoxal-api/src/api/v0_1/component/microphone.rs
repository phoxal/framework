/// One audio frame as raw encoded bytes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub data: Vec<u8>,
}

phoxal_macros::phoxal_api_fragment! {
    path component(instance) / microphone(capability);

    version v0_1;

    topic frame: Sample<Frame>;
}
