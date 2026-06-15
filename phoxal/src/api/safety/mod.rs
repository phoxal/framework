pub mod v1;

contract! {
    pub enum SafetyAuthorization {
        "1" => V1(v1::SafetyAuthorization),
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
    pub enum EmergencyStopRequest {
        "1" => V1(v1::EmergencyStopRequest),
    }
}

contract! {
    pub enum Evidence {
        "1" => V1(v1::Evidence),
    }
}

contract! {
    pub enum StopSet {
        "1" => V1(v1::StopSet),
    }
}

contract! {
    pub enum LatencyBudget {
        "1" => V1(v1::LatencyBudget),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SourceHealth {
        "1" => V1(v1::SourceHealth),
    }
}
