pub mod v1;

contract! {
    pub enum Frontiers {
        "1" => V1(v1::Frontiers),
    }
}

contract! {
    pub enum GoalCandidates {
        "1" => V1(v1::GoalCandidates),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        "1" => V1(v1::State),
    }
}

contract! {
    pub enum Scoring {
        "1" => V1(v1::Scoring),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RejectedCandidates {
        "1" => V1(v1::RejectedCandidates),
    }
}
