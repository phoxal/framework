/// One chunk of an audio stream to play on this speaker.
///
/// `Some(bytes)` carries WAV-coded audio: the first chunk of a
/// stream starts with the standard WAV header, later chunks
/// continue its data. `None` ends the stream and is what tells
/// the owner the sound is complete.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Chunk {
    pub stream: Option<Vec<u8>>,
}

phoxal_macros::phoxal_api_fragment! {
    path robot / component(instance) / speaker(capability);

    command stream: Stream<Chunk>;
}
