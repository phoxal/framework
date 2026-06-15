pub mod v1;

contract! {
    pub enum OdometryEstimate {
        "1" => V1(v1::OdometryEstimate),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Status {
        "1" => V1(v1::Status),
    }
}

contract! {
    #[derive(Eq)]
    pub enum SourceHealth {
        "1" => V1(v1::SourceHealth),
    }
}

contract! {
    pub enum Residuals {
        "1" => V1(v1::Residuals),
    }
}

contract! {
    pub enum Integration {
        "1" => V1(v1::Integration),
    }
}
