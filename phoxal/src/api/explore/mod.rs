pub mod v1;

contract! {
    pub enum Frontiers {
        V1(v1::Frontiers),
    }
}

contract! {
    pub enum GoalCandidates {
        V1(v1::GoalCandidates),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        V1(v1::State),
    }
}

contract! {
    pub enum Scoring {
        V1(v1::Scoring),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RejectedCandidates {
        V1(v1::RejectedCandidates),
    }
}
