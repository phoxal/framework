pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum Heartbeat {
        V1(v1::Heartbeat),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Summary {
        V1(v1::Summary),
    }
}

contract! {
    #[derive(Eq)]
    pub enum DebugReadiness {
        V1(v1::DebugReadiness),
    }
}
