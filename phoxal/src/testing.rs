//! Explicit in-process participant testing support.
//!
//! This module is the whole of the `test-harness` profile. It is not a process
//! launch protocol: a test opens a [`TestBus`], supplies it to
//! [`run_test_harness`], and drives the participant's lifecycle explicitly.
//! Production participants enter through [`crate::run`], which parses the
//! supervised launch and opens its own unique bus owner.
//!
//! Everything below is defined in the module that owns it and re-exported
//! here. This module is the profile: the definitions themselves are compiled
//! in every build, because a domain module never asks which profile it is in.

pub use crate::identity::TimelineId;
pub use crate::participant::clock::ClockSource;
pub use crate::participant::clock::test::TestClock;
pub use crate::participant::runner::harness::{TestBus, TestHarness, step_token};
pub use crate::participant::runner::{run_test_harness, run_test_harness_with_clock};
