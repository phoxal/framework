pub mod v1;

contract! {
    pub enum State {
        V1(v1::State),
    }
}

contract! {
    pub enum ManualCommand {
        V1(v1::ManualCommand),
    }
}

contract! {
    pub enum Arbitration {
        V1(v1::Arbitration),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SourceFreshness {
        V1(v1::SourceFreshness),
    }
}
