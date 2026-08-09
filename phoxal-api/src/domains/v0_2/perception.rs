//! v0.2 perception payloads.
#![allow(legacy_derive_helpers)]

            /// A current-revision detection with wire-level finite and fixed
            /// shape guarantees. The v0.1 body above remains untouched.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            pub struct Detection {
                pub class_id: String,
                #[serde(deserialize_with = "crate::deserialize_finite_detection_confidence")]
                pub confidence: f32,
                #[serde(deserialize_with = "crate::deserialize_finite_detection_position")]
                pub position_m: [f64; 3],
                pub frame_id: String,
                pub track_id: Option<u64>,
            }

            /// One source-captured perception batch. `captured_at` is copied
            /// from the selected camera measurement's provenance; it is not
            /// the perception step's publication instant.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            pub struct Detections {
                pub source: crate::SourceRef,
                pub captured_at: ::phoxal_bus::TimeWindow,
                pub detections: Vec<Detection>,
            }

            /// Why the perception participant cannot provide a healthy batch.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum HealthReason {
                MissingCamera,
                StaleCamera,
                InvalidCamera,
                DetectorFailure,
                BackendUnavailable,
                PublicationFailure,
                ManagedInputFailure,
            }

            /// The perception participant's exclusive published health.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            pub enum State {
                Healthy { detector: String },
                Unhealthy {
                    detector: String,
                    reason: HealthReason,
                },
            }

