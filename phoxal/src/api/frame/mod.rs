pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Tree {
        V1(v1::Tree),
    }
}

contract! {
    pub enum Static {
        V1(v1::Static),
    }
}

contract! {
    pub enum FrameTransform {
        V1(v1::FrameTransform),
    }
}

contract! {
    #[derive(Eq)]
    pub enum FrameLookupRequest {
        V1(v1::FrameLookupRequest),
    }
}

contract! {
    pub enum FrameLookupResponse {
        V1(v1::FrameLookupResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for FrameLookupResponse {
    fn busy() -> Self {
        Self::V1(<v1::FrameLookupResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
