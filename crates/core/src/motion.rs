/// Analytic critically damped spring. Retargeting preserves velocity; evaluation
/// is independent of refresh rate and remains stable after a delayed frame.
pub struct Spring {
    /// Current position, read by the renderer after stepping.
    pub value: f32,
    velocity: f32,
    /// Destination; changing it preserves the current position and velocity.
    pub target: f32,
}

impl Spring {
    /// Start at rest at `value`.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    /// Advances by `seconds`; a higher positive `omega` settles sooner.
    pub fn step(&mut self, seconds: f32, omega: f32) {
        let seconds = seconds.max(0.0);
        let displacement = self.value - self.target;
        let c = omega.mul_add(displacement, self.velocity);
        let decay = (-omega * seconds).exp();
        self.value = c.mul_add(seconds, displacement).mul_add(decay, self.target);
        self.velocity = (omega * c).mul_add(-seconds, self.velocity) * decay;
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    /// Whether position and velocity are within the rendering tolerance.
    #[must_use]
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 0.05 && self.velocity.abs() < 0.1
    }

    /// Settle immediately at the target for reduced-motion rendering.
    pub fn snap(&mut self) {
        self.value = self.target;
        self.velocity = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_elapsed_time_matches_zero_without_reversing_motion() {
        let mut negative = Spring::new(56.0);
        let mut zero = Spring::new(56.0);
        for spring in [&mut negative, &mut zero] {
            spring.target = 188.0;
            spring.step(0.05, 24.0);
        }
        negative.step(-0.1, 24.0);
        zero.step(0.0, 24.0);
        assert_eq!(negative.value, zero.value);
        assert_eq!(negative.velocity, zero.velocity);
        negative.step(0.05, 24.0);
        zero.step(0.05, 24.0);
        assert_eq!(negative.value, zero.value);
        assert_eq!(negative.velocity, zero.velocity);
    }

    #[test]
    fn interrupted_motion_is_continuous_and_refresh_rate_independent() {
        let mut a = Spring::new(56.0);
        let mut b = Spring::new(56.0);
        a.target = 188.0;
        b.target = 188.0;
        for _ in 0..12 {
            a.step(1.0 / 120.0, 24.0);
        }
        for _ in 0..6 {
            b.step(1.0 / 60.0, 24.0);
        }
        assert!((a.value - b.value).abs() < 0.001);
        let value = a.value;
        a.target = 72.0;
        assert_eq!(a.value, value);
        a.step(4.0, 24.0);
        assert_eq!(a.value, 72.0);
    }
}
