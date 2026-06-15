pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Tree {
        "1" => V1(v1::Tree),
    }
}

contract! {
    pub enum Static {
        "1" => V1(v1::Static),
    }
}

contract! {
    pub enum FrameTransform {
        "1" => V1(v1::FrameTransform),
    }
}

contract! {
    #[derive(Eq)]
    pub enum FrameLookupRequest {
        "1" => V1(v1::FrameLookupRequest),
    }
}

contract! {
    pub enum FrameLookupResponse {
        "1" => V1(v1::FrameLookupResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for FrameLookupResponse {
    fn busy() -> Self {
        Self::V1(<v1::FrameLookupResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
