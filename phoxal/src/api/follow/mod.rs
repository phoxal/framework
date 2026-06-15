pub mod v1;

contract! {
    pub enum Target {
        V1(v1::Target),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        V1(v1::State),
    }
}

contract! {
    pub enum TrackingError {
        V1(v1::TrackingError),
    }
}

contract! {
    pub enum Candidates {
        V1(v1::Candidates),
    }
}

contract! {
    pub enum Costs {
        V1(v1::Costs),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RevisionInputs {
        V1(v1::RevisionInputs),
    }
}
