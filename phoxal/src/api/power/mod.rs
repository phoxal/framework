pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Command {
        "1" => V1(v1::Command),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        "1" => V1(v1::State),
    }
}
