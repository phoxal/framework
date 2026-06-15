pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Clock {
        "1" => V1(v1::Clock),
    }
}
