/// Analytic critically damped spring. Retargeting preserves velocity; evaluation
/// is independent of refresh rate and remains stable after a delayed frame.
pub struct Spring {
    pub value: f32,
    velocity: f32,
    pub target: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    pub fn step(&mut self, seconds: f32) {
        let omega = 24.0;
        let displacement = self.value - self.target;
        let c = self.velocity + omega * displacement;
        let decay = (-omega * seconds.max(0.0)).exp();
        self.value = self.target + (displacement + c * seconds) * decay;
        self.velocity = (self.velocity - omega * c * seconds) * decay;
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 0.05 && self.velocity.abs() < 0.1
    }

    pub fn snap(&mut self) {
        self.value = self.target;
        self.velocity = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_motion_is_continuous_and_refresh_rate_independent() {
        let mut a = Spring::new(56.0);
        let mut b = Spring::new(56.0);
        a.target = 188.0;
        b.target = 188.0;
        for _ in 0..12 {
            a.step(1.0 / 120.0);
        }
        for _ in 0..6 {
            b.step(1.0 / 60.0);
        }
        assert!((a.value - b.value).abs() < 0.001);
        let value = a.value;
        a.target = 72.0;
        assert_eq!(a.value, value);
        a.step(4.0);
        assert_eq!(a.value, 72.0);
    }
}
