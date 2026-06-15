pub mod v1;

contract! {
    pub enum MissionCommand {
        "1" => V1(v1::MissionCommand),
    }
}

contract! {
    pub enum State {
        "1" => V1(v1::State),
    }
}

contract! {
    pub enum Goal {
        "1" => V1(v1::Goal),
    }
}

contract! {
    #[derive(Eq)]
    pub enum DecisionTrace {
        "1" => V1(v1::DecisionTrace),
    }
}
