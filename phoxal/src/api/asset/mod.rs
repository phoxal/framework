pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum GetRequest {
        "1" => V1(v1::GetRequest),
    }
}

contract! {
    #[derive(Eq)]
    pub enum GetResponse {
        "1" => V1(v1::GetResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for GetResponse {
    fn busy() -> Self {
        Self::V1(<v1::GetResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
