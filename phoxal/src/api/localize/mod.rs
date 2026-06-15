pub mod v1;

contract! {
    pub enum LocalizationState {
        "1" => V1(v1::LocalizationState),
    }
}

contract! {
    pub enum PoseEstimate {
        "1" => V1(v1::PoseEstimate),
    }
}

contract! {
    pub enum LocalizationRevision {
        "1" => V1(v1::LocalizationRevision),
    }
}

contract! {
    pub enum Keyframe {
        "1" => V1(v1::Keyframe),
    }
}

contract! {
    pub enum PoseGraphCorrection {
        "1" => V1(v1::PoseGraphCorrection),
    }
}

contract! {
    pub enum PoseGraphRequest {
        "1" => V1(v1::PoseGraphRequest),
    }
}

contract! {
    pub enum PoseGraphResponse {
        "1" => V1(v1::PoseGraphResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for PoseGraphResponse {
    fn busy() -> Self {
        Self::V1(<v1::PoseGraphResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum KeyframeRequest {
        "1" => V1(v1::KeyframeRequest),
    }
}

contract! {
    pub enum KeyframeResponse {
        "1" => V1(v1::KeyframeResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for KeyframeResponse {
    fn busy() -> Self {
        Self::V1(<v1::KeyframeResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum CorrectionsRequest {
        "1" => V1(v1::CorrectionsRequest),
    }
}

contract! {
    pub enum CorrectionsResponse {
        "1" => V1(v1::CorrectionsResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for CorrectionsResponse {
    fn busy() -> Self {
        Self::V1(<v1::CorrectionsResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
