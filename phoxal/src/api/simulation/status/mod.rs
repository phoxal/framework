pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Status {
        "1" => V1(v1::Status),
    }
}
