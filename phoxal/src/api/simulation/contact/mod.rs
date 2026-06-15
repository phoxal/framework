pub mod v1;

contract! {
    pub enum Contact {
        V1(v1::Contact),
    }
}
