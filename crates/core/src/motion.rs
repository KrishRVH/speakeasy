use std::time::Duration;

const POSITION_TOLERANCE: f32 = 0.05;
const VELOCITY_TOLERANCE: f32 = 0.1;

#[derive(Debug)]
pub struct Spring {
    value: f32,
    velocity: f32,
    pub target: f32,
}

impl Spring {
    #[must_use]
    pub const fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    #[must_use]
    pub const fn value(&self) -> f32 {
        self.value
    }

    pub fn step(&mut self, elapsed: Duration, omega: f32) {
        let seconds = elapsed.as_secs_f32();
        let displacement = self.value - self.target;
        let slope = self.velocity + omega * displacement;
        let decay = (-omega * seconds).exp();
        self.value = self.target + (displacement + slope * seconds) * decay;
        self.velocity = (self.velocity - omega * slope * seconds) * decay;
        if self.settled() {
            self.snap();
        }
    }

    #[must_use]
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < POSITION_TOLERANCE
            && self.velocity.abs() < VELOCITY_TOLERANCE
    }

    pub const fn snap(&mut self) {
        self.value = self.target;
        self.velocity = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Spring;

    const OMEGA: f32 = 24.0;

    #[test]
    fn interrupted_motion_is_continuous_and_refresh_rate_independent() {
        let mut fast = Spring::new(56.0);
        let mut slow = Spring::new(56.0);
        fast.target = 188.0;
        slow.target = 188.0;
        for _ in 0..12 {
            fast.step(Duration::from_secs(1) / 120, OMEGA);
        }
        for _ in 0..6 {
            slow.step(Duration::from_secs(1) / 60, OMEGA);
        }
        assert!((fast.value() - slow.value()).abs() < 0.001);

        let interrupted_at = fast.value();
        fast.target = 72.0;
        fast.step(Duration::from_secs(1) / 120, OMEGA);
        assert!(fast.value() > interrupted_at, "{}", fast.value());
        fast.step(Duration::from_secs(4), OMEGA);
        assert_eq!(fast.value(), 72.0);
    }
}
