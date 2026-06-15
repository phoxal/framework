pub mod v1;

contract! {
    pub enum Depth {
        "1" => V1(v1::Depth),
    }
}
