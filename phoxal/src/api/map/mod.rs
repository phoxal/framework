pub mod v1;

contract! {
    pub enum MapRevision {
        "1" => V1(v1::MapRevision),
    }
}

contract! {
    pub enum Summary {
        "1" => V1(v1::Summary),
    }
}

contract! {
    pub enum LocalCost {
        "1" => V1(v1::LocalCost),
    }
}

contract! {
    pub enum Traversability {
        "1" => V1(v1::Traversability),
    }
}

contract! {
    pub enum TraversabilitySummary {
        "1" => V1(v1::TraversabilitySummary),
    }
}

contract! {
    pub enum SubmapRequest {
        "1" => V1(v1::SubmapRequest),
    }
}

contract! {
    pub enum SubmapResponse {
        "1" => V1(v1::SubmapResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for SubmapResponse {
    fn busy() -> Self {
        Self::V1(<v1::SubmapResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum EsdfTileRequest {
        "1" => V1(v1::EsdfTileRequest),
    }
}

contract! {
    pub enum EsdfTileResponse {
        "1" => V1(v1::EsdfTileResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for EsdfTileResponse {
    fn busy() -> Self {
        Self::V1(<v1::EsdfTileResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum TraversabilityTileRequest {
        "1" => V1(v1::TraversabilityTileRequest),
    }
}

contract! {
    pub enum TraversabilityTileResponse {
        "1" => V1(v1::TraversabilityTileResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for TraversabilityTileResponse {
    fn busy() -> Self {
        Self::V1(<v1::TraversabilityTileResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum LocalGridRequest {
        "1" => V1(v1::LocalGridRequest),
    }
}

contract! {
    pub enum LocalGridResponse {
        "1" => V1(v1::LocalGridResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for LocalGridResponse {
    fn busy() -> Self {
        Self::V1(<v1::LocalGridResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum GlobalGridRequest {
        "1" => V1(v1::GlobalGridRequest),
    }
}

contract! {
    pub enum GlobalGridResponse {
        "1" => V1(v1::GlobalGridResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for GlobalGridResponse {
    fn busy() -> Self {
        Self::V1(<v1::GlobalGridResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum SnapshotRequest {
        "1" => V1(v1::SnapshotRequest),
    }
}

contract! {
    pub enum SnapshotResponse {
        "1" => V1(v1::SnapshotResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for SnapshotResponse {
    fn busy() -> Self {
        Self::V1(<v1::SnapshotResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
