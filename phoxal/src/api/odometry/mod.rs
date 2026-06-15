pub mod v1;

contract! {
    pub enum OdometryEstimate {
        V1(v1::OdometryEstimate),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Status {
        V1(v1::Status),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SourceHealth {
        V1(v1::SourceHealth),
    }
}

contract! {
    pub enum Residuals {
        V1(v1::Residuals),
    }
}

contract! {
    pub enum Integration {
        V1(v1::Integration),
    }
}
