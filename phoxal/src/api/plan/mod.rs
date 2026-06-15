pub mod v1;

contract! {
    pub enum Path {
        V1(v1::Path),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        V1(v1::State),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SearchGraph {
        V1(v1::SearchGraph),
    }
}

contract! {
    pub enum CostLayers {
        V1(v1::CostLayers),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RejectedPaths {
        V1(v1::RejectedPaths),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RevisionInputs {
        V1(v1::RevisionInputs),
    }
}
