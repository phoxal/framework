use super::*;

pub(super) struct EncoderDevice {
    pub(super) native: webots_rs::device::position_sensor::PositionSensor,
    pub(super) gear_ratio: f64,
    pub(super) schedule: SampleSchedule,
    pub(super) last: Option<(f64, u64)>,
    pub(super) publisher: LiveSamplePublisher<api::component::encoder::Sample>,
}

impl EncoderDevice {
    pub(super) fn publish_output(&mut self, transition: &LiveTransitionStamp) -> Result<()> {
        let elapsed_ns = transition.progress().elapsed_ns();
        if !self.schedule.is_due_at(elapsed_ns)? {
            return Ok(());
        }
        let position = self.native.value()? * self.gear_ratio;
        let velocity = self
            .last
            .map(|(previous, time)| {
                let delta = elapsed_ns.saturating_sub(time);
                if delta == 0 {
                    0.0
                } else {
                    (position - previous) * 1_000_000_000.0 / delta as f64
                }
            })
            .unwrap_or(0.0);
        self.last = Some((position, elapsed_ns));
        self.publisher.publish(
            transition,
            api::component::encoder::Sample::try_new(position, velocity as f32)?,
        )?;
        Ok(())
    }
}
