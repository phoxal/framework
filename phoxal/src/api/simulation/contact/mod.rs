pub mod v1;

contract! {
    pub enum Contact {
        "1" => V1(v1::Contact),
    }
}
