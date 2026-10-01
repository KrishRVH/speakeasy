//! Analytic critically damped springs for frame-rate independent animation.

use std::time::Duration;

const POSITION_TOLERANCE: f32 = 0.05;
const VELOCITY_TOLERANCE: f32 = 0.1;

/// A critically damped spring evaluated in closed form.
///
/// Stepping is independent of refresh rate and stays stable after a delayed frame. Retargeting
/// keeps the current position and velocity, so interrupted motion stays continuous.
#[derive(Debug)]
pub struct Spring {
    value: f32,
    velocity: f32,
    /// Destination the spring settles toward.
    pub target: f32,
}

impl Spring {
    /// Creates a spring resting at `value`.
    #[must_use]
    pub const fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    /// Returns the current position.
    #[must_use]
    pub const fn value(&self) -> f32 {
        self.value
    }

    /// Advances by `elapsed`; a higher positive `omega` settles sooner.
    pub fn step(&mut self, elapsed: Duration, omega: f32) {
        // With displacement x₀ and velocity v₀: x(t) = (x₀ + slope·t)·e^(−ωt) and
        // v(t) = (v₀ − ω·slope·t)·e^(−ωt), where slope = v₀ + ω·x₀.
        let seconds = elapsed.as_secs_f32();
        let displacement = self.value - self.target;
        let slope = omega.mul_add(displacement, self.velocity);
        let decay = (-omega * seconds).exp();
        self.value = slope
            .mul_add(seconds, displacement)
            .mul_add(decay, self.target);
        self.velocity = (omega * slope).mul_add(-seconds, self.velocity) * decay;
        if self.settled() {
            self.snap();
        }
    }

    /// Whether position and velocity are within the rendering tolerance.
    #[must_use]
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < POSITION_TOLERANCE
            && self.velocity.abs() < VELOCITY_TOLERANCE
    }

    /// Settles immediately at the target, for reduced-motion rendering.
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
        assert_eq!(fast.value(), interrupted_at);
        fast.step(Duration::from_secs(4), OMEGA);
        assert_eq!(fast.value(), 72.0);
    }
}
