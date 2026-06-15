pub mod v1;

contract! {
    pub enum MapRevision {
        V1(v1::MapRevision),
    }
}

contract! {
    pub enum Summary {
        V1(v1::Summary),
    }
}

contract! {
    pub enum LocalCost {
        V1(v1::LocalCost),
    }
}

contract! {
    pub enum Traversability {
        V1(v1::Traversability),
    }
}

contract! {
    pub enum TraversabilitySummary {
        V1(v1::TraversabilitySummary),
    }
}

contract! {
    pub enum SubmapRequest {
        V1(v1::SubmapRequest),
    }
}

contract! {
    pub enum SubmapResponse {
        V1(v1::SubmapResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for SubmapResponse {
    fn busy() -> Self {
        Self::V1(<v1::SubmapResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum EsdfTileRequest {
        V1(v1::EsdfTileRequest),
    }
}

contract! {
    pub enum EsdfTileResponse {
        V1(v1::EsdfTileResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for EsdfTileResponse {
    fn busy() -> Self {
        Self::V1(<v1::EsdfTileResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum TraversabilityTileRequest {
        V1(v1::TraversabilityTileRequest),
    }
}

contract! {
    pub enum TraversabilityTileResponse {
        V1(v1::TraversabilityTileResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for TraversabilityTileResponse {
    fn busy() -> Self {
        Self::V1(<v1::TraversabilityTileResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum LocalGridRequest {
        V1(v1::LocalGridRequest),
    }
}

contract! {
    pub enum LocalGridResponse {
        V1(v1::LocalGridResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for LocalGridResponse {
    fn busy() -> Self {
        Self::V1(<v1::LocalGridResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum GlobalGridRequest {
        V1(v1::GlobalGridRequest),
    }
}

contract! {
    pub enum GlobalGridResponse {
        V1(v1::GlobalGridResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for GlobalGridResponse {
    fn busy() -> Self {
        Self::V1(<v1::GlobalGridResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

contract! {
    pub enum SnapshotRequest {
        V1(v1::SnapshotRequest),
    }
}

contract! {
    pub enum SnapshotResponse {
        V1(v1::SnapshotResponse),
    }
}

impl crate::bus::zenoh::BusyResponse for SnapshotResponse {
    fn busy() -> Self {
        Self::V1(<v1::SnapshotResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
