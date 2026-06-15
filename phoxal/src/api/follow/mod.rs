pub mod v1;

contract! {
    pub enum Target {
        "1" => V1(v1::Target),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        "1" => V1(v1::State),
    }
}

contract! {
    pub enum TrackingError {
        "1" => V1(v1::TrackingError),
    }
}

contract! {
    pub enum Candidates {
        "1" => V1(v1::Candidates),
    }
}

contract! {
    pub enum Costs {
        "1" => V1(v1::Costs),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RevisionInputs {
        "1" => V1(v1::RevisionInputs),
    }
}
