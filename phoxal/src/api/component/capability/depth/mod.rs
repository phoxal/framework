pub mod v1;

contract! {
    pub enum Depth {
        V1(v1::Depth),
    }
}
