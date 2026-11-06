const TICK_SECONDS: f64 = 1.0 / 20.0;
const MAX_FRAME_DELTA_SECONDS: f64 = 0.25;
const MAX_TICKS_PER_FRAME: u32 = 5;

/// Report from advancing the fixed-step simulation clock.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickReport {
    pub ticks_run: u32,
    pub interpolation_alpha: f64,
    pub dropped_seconds: f64,
}

/// Fixed 20 TPS accumulator independent from the display's frame rate.
///
/// A long tab suspension is clamped to five ticks per animation frame so
/// browser resumption cannot trigger an unbounded catch-up loop.
#[derive(Clone, Debug, Default)]
pub struct FixedTickClock {
    accumulator_seconds: f64,
    tick_index: u64,
}

impl FixedTickClock {
    pub const fn new() -> Self {
        Self {
            accumulator_seconds: 0.0,
            tick_index: 0,
        }
    }

    pub fn tick_index(&self) -> u64 {
        self.tick_index
    }

    pub fn tick_seconds() -> f64 {
        TICK_SECONDS
    }

    pub fn advance(&mut self, elapsed_seconds: f64, mut tick: impl FnMut(u64)) -> TickReport {
        if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
            return TickReport {
                interpolation_alpha: self.interpolation_alpha(),
                ..TickReport::default()
            };
        }

        let accepted = elapsed_seconds.min(MAX_FRAME_DELTA_SECONDS);
        let mut report = TickReport {
            dropped_seconds: (elapsed_seconds - accepted).max(0.0),
            ..TickReport::default()
        };
        self.accumulator_seconds += accepted;

        while self.accumulator_seconds >= TICK_SECONDS
            && report.ticks_run < MAX_TICKS_PER_FRAME
        {
            self.accumulator_seconds -= TICK_SECONDS;
            self.tick_index = self.tick_index.wrapping_add(1);
            tick(self.tick_index);
            report.ticks_run += 1;
        }

        // Floating point rounding must never make the interpolation value
        // negative or greater than one fixed simulation step.
        self.accumulator_seconds = self.accumulator_seconds.clamp(0.0, TICK_SECONDS);
        report.interpolation_alpha = self.interpolation_alpha();
        report
    }

    pub fn interpolation_alpha(&self) -> f64 {
        (self.accumulator_seconds / TICK_SECONDS).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::FixedTickClock;

    #[test]
    fn advances_at_twenty_ticks_per_second_independent_of_frame_rate() {
        let mut clock = FixedTickClock::new();
        let mut indices = Vec::new();
        for _ in 0..60 {
            clock.advance(1.0 / 60.0, |index| indices.push(index));
        }
        assert_eq!(indices.len(), 20);
        assert_eq!(indices[0], 1);
        assert_eq!(indices[19], 20);
        assert_eq!(clock.tick_index(), 20);
    }

    #[test]
    fn catches_up_when_render_rate_is_higher_than_tick_rate() {
        let mut clock = FixedTickClock::new();
        let mut total = 0;
        for _ in 0..120 {
            total += clock.advance(1.0 / 120.0, |_| {}).ticks_run;
        }
        assert_eq!(total, 20);
    }

    #[test]
    fn caps_long_frame_catchup_to_five_ticks() {
        let mut clock = FixedTickClock::new();
        let report = clock.advance(4.0, |_| {});
        assert_eq!(report.ticks_run, 5);
        assert_eq!(report.dropped_seconds, 3.75);
        assert!(report.interpolation_alpha < 1.0);
    }

    #[test]
    fn ignores_invalid_elapsed_time() {
        let mut clock = FixedTickClock::new();
        assert_eq!(clock.advance(f64::NAN, |_| {}).ticks_run, 0);
        assert_eq!(clock.advance(-1.0, |_| {}).ticks_run, 0);
        assert_eq!(clock.tick_index(), 0);
    }
}
