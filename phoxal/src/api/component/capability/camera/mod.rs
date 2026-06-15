pub mod v1;

contract! {
    pub enum Frame {
        V1(v1::Frame),
    }
}
