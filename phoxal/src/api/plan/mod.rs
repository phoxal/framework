pub mod v1;

contract! {
    pub enum Path {
        "1" => V1(v1::Path),
    }
}

contract! {
    #[derive(Eq)]
    pub enum State {
        "1" => V1(v1::State),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SearchGraph {
        "1" => V1(v1::SearchGraph),
    }
}

contract! {
    pub enum CostLayers {
        "1" => V1(v1::CostLayers),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RejectedPaths {
        "1" => V1(v1::RejectedPaths),
    }
}

contract! {
    #[derive(Eq)]
    pub enum RevisionInputs {
        "1" => V1(v1::RevisionInputs),
    }
}
