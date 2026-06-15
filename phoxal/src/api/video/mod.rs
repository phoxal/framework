pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum OpenRequest {
        V1(v1::OpenRequest),
    }
}

contract! {
    #[derive(Eq)]
    pub enum OpenResponse {
        V1(v1::OpenResponse),
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
        V1(v1::StreamEvent),
    }
}
