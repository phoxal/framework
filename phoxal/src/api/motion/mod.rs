pub mod v1;

contract! {
    pub enum State {
        "1" => V1(v1::State),
    }
}

contract! {
    pub enum ManualCommand {
        "1" => V1(v1::ManualCommand),
    }
}

contract! {
    pub enum Arbitration {
        "1" => V1(v1::Arbitration),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SourceFreshness {
        "1" => V1(v1::SourceFreshness),
    }
}
