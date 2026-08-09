//! v0.1 power payloads.
#![allow(legacy_derive_helpers)]

            /// A platform power command.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Command {
                Reboot,
                Shutdown,
            }

            /// Where the power participant is in handling a command.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub enum Status {
                Idle,
                Rebooting,
                ShuttingDown,
                Failed,
            }

            /// Why a power command was rejected outright.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum RejectedReason {
                HostIntegrationUnavailable,
                CommandRejected,
            }

            /// Why an accepted power command later failed.
            #[derive(Copy, Eq)]
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum FailedReason {
                HostCommandFailed,
            }

            /// The power participant's published state.
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct State {
                pub status: Status,
                pub detail: Option<String>,
            }


