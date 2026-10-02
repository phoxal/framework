//! The multistage service's authored contract: a command-triggered
//! procedure with one generated survey call, stage events, and a retained
//! status publication.

#[phoxal::messages(package = "phoxal.tests.authoring.multistage.v1")]
mod v1 {
    use phoxal::contracts::{Latest, Queue, RequestReply};

    pub struct BeginRequest {
        #[phoxal(tag = 1)]
        pub job_id: u64,
    }

    pub enum BeginResponse {
        #[phoxal(tag = 1)]
        Accepted,
        #[phoxal(tag = 2)]
        Busy,
        #[phoxal(tag = 3)]
        Invalid,
    }

    pub struct StopRequest {
        #[phoxal(tag = 1)]
        pub job_id: u64,
    }

    pub enum StopResponse {
        #[phoxal(tag = 1)]
        Stopped,
        #[phoxal(tag = 2)]
        NotRunning,
    }

    pub struct SurveyRequest {
        #[phoxal(tag = 1)]
        pub pass: u64,
    }

    pub struct SurveyResponse {
        #[phoxal(tag = 1)]
        pub reading: u64,
    }

    pub struct StageEvent {
        #[phoxal(tag = 1)]
        pub job_id: u64,
        #[phoxal(tag = 2)]
        pub stage: u32,
    }

    pub struct MissionState {
        #[phoxal(tag = 1)]
        pub job_id: Option<u64>,
        #[phoxal(tag = 2)]
        pub phase: Phase,
    }

    pub enum Phase {
        Unspecified = 0,
        Idle = 1,
        Surveying = 2,
        Settling = 3,
        Succeeded = 4,
        Cancelled = 5,
    }

    /// The multistage service's endpoint contract.
    #[phoxal::endpoints]
    pub struct MultistageApi {
        #[phoxal::operation(max_items = 8, max_bytes = 1_024)]
        begin: RequestReply<BeginRequest, BeginResponse>,

        #[phoxal::operation(max_items = 8, max_bytes = 1_024)]
        stop: RequestReply<StopRequest, StopResponse>,

        #[phoxal::call(
            contract = "phoxal.tests.authoring.multistage.v1.Survey",
            max_items = 8,
            max_bytes = 1_024
        )]
        survey: RequestReply<SurveyRequest, SurveyResponse>,

        #[phoxal::output(max_items = 64, max_bytes = 4_096)]
        stages: Queue<StageEvent>,

        #[phoxal::output(projection = state, bootstrap, max_bytes = 512)]
        status: Latest<MissionState>,
    }
}

pub use v1::*;
