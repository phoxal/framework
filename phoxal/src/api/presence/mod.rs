pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Heartbeat {
        "1" => V1(v1::Heartbeat),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Summary {
        "1" => V1(v1::Summary),
    }
}

contract! {
    #[derive(Eq)]
    pub enum DebugReadiness {
        "1" => V1(v1::DebugReadiness),
    }
}
