pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Status {
        V1(v1::Status),
    }
}
