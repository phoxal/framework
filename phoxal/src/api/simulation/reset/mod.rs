pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Request {
        "1" => V1(v1::Request),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Response {
        "1" => V1(v1::Response),
    }
}
