pub mod v1;

contract! {
    pub enum SafetyAuthorization {
        V1(v1::SafetyAuthorization),
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
    pub enum EmergencyStopRequest {
        V1(v1::EmergencyStopRequest),
    }
}

contract! {
    pub enum Evidence {
        V1(v1::Evidence),
    }
}

contract! {
    pub enum StopSet {
        V1(v1::StopSet),
    }
}

contract! {
    pub enum LatencyBudget {
        V1(v1::LatencyBudget),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SourceHealth {
        V1(v1::SourceHealth),
    }
}
