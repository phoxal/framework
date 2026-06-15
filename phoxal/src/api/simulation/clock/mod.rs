pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Clock {
        V1(v1::Clock),
    }
}
