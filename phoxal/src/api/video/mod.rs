pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum OpenRequest {
        "1" => V1(v1::OpenRequest),
    }
}

contract! {
    #[derive(Eq)]
    pub enum OpenResponse {
        "1" => V1(v1::OpenResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for OpenResponse {
    fn busy() -> Self {
        Self::V1(<v1::OpenResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    #[derive(Eq)]
    pub enum StreamEvent {
        "1" => V1(v1::StreamEvent),
    }
}
