pub mod v1;

contract! {
    pub enum Frame {
        "1" => V1(v1::Frame),
    }
}
