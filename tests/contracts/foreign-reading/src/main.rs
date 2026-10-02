//! Foreign-vocabulary producer: the same physical quantities as the standard
//! encoder contract under different names and wire numbers, plus a harmless
//! diagnostics field the receiver-side mapping deliberately omits.

use phoxal::contracts::Latest;
use phoxal::runtime::Context;

#[phoxal::message(package = "example.foreign.v1")]
pub struct ForeignReading {
    #[phoxal(tag = 2)]
    pub shaft_rate_radps: Option<f64>,
    #[phoxal(tag = 5)]
    pub shaft_position_rad: Option<f64>,
    #[phoxal(tag = 9)]
    pub diagnostics: Option<String>,
}

/// The foreign reading contract.
#[phoxal::endpoints]
pub struct ForeignApi {
    #[phoxal::output(max_bytes = 1024)]
    reading: Latest<ForeignReading>,
}

#[derive(Debug)]
struct ForeignReadingService {
    step: u64,
}

#[phoxal::runtime(contract = ForeignApi, period_ms = 20)]
impl ForeignReadingService {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self { step: 0 })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.step = self.step.saturating_add(1);
        let mut reading = ForeignReading {
            shaft_rate_radps: Some(0.5),
            shaft_position_rad: Some(
                5.0 + f64::from(u32::try_from(self.step).unwrap_or(u32::MAX)) * 0.001,
            ),
            diagnostics: None,
        };
        // Absence stays absence: the rate is omitted for the first ten steps.
        if self.step < 10 {
            reading.shaft_rate_radps = None;
        }
        if self.step.is_multiple_of(100) {
            reading.diagnostics = Some("periodic".to_owned());
        }
        ctx.publish_reading(reading)?;
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<ForeignReadingService>()
}
