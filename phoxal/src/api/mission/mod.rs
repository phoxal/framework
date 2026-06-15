pub mod v1;

contract! {
    pub enum MissionCommand {
        V1(v1::MissionCommand),
    }
}

contract! {
    pub enum State {
        V1(v1::State),
    }
}

contract! {
    pub enum Goal {
        V1(v1::Goal),
    }
}

contract! {
    #[derive(Eq)]
    pub enum DecisionTrace {
        V1(v1::DecisionTrace),
    }
}
