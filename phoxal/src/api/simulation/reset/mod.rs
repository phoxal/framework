pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Request {
        V1(v1::Request),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Response {
        V1(v1::Response),
    }
}
