pub mod v1;

contract! {
    pub enum LocalizationState {
        V1(v1::LocalizationState),
    }
}

contract! {
    pub enum PoseEstimate {
        V1(v1::PoseEstimate),
    }
}

contract! {
    pub enum LocalizationRevision {
        V1(v1::LocalizationRevision),
    }
}

contract! {
    pub enum Keyframe {
        V1(v1::Keyframe),
    }
}

contract! {
    pub enum PoseGraphCorrection {
        V1(v1::PoseGraphCorrection),
    }
}

contract! {
    pub enum PoseGraphRequest {
        V1(v1::PoseGraphRequest),
    }
}

contract! {
    pub enum PoseGraphResponse {
        V1(v1::PoseGraphResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for PoseGraphResponse {
    fn busy() -> Self {
        Self::V1(<v1::PoseGraphResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum KeyframeRequest {
        V1(v1::KeyframeRequest),
    }
}

contract! {
    pub enum KeyframeResponse {
        V1(v1::KeyframeResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for KeyframeResponse {
    fn busy() -> Self {
        Self::V1(<v1::KeyframeResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum CorrectionsRequest {
        V1(v1::CorrectionsRequest),
    }
}

contract! {
    pub enum CorrectionsResponse {
        V1(v1::CorrectionsResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for CorrectionsResponse {
    fn busy() -> Self {
        Self::V1(<v1::CorrectionsResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
