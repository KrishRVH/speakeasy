use std::f64::consts::TAU;

const QUALITIES: [f64; 4] = [
    0.509_795_579_104_159_2,
    0.601_344_886_935_045_3,
    0.899_976_223_136_415_6,
    2.562_915_447_741_505_5,
];

pub(crate) struct Lowpass([Biquad; 4]);

impl Lowpass {
    pub(crate) fn new(cutoff: f64, rate: f64) -> Option<Self> {
        (cutoff > 0.0 && cutoff < rate / 2.0)
            .then(|| Self(QUALITIES.map(|quality| Biquad::new(TAU * cutoff / rate, quality))))
    }

    pub(crate) fn filter(&mut self, sample: f64) -> f64 {
        self.0
            .iter_mut()
            .fold(sample, |sample, section| section.filter(sample))
    }
}

struct Biquad {
    b0: f64,
    b1: f64,
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl Biquad {
    fn new(omega: f64, quality: f64) -> Self {
        let alpha = omega.sin() / (2.0 * quality);
        let one_minus_cos = 2.0 * (omega / 2.0).sin().powi(2);
        let a0 = 1.0 + alpha;
        Self {
            b0: one_minus_cos / 2.0 / a0,
            b1: one_minus_cos / a0,
            a1: -2.0 * omega.cos() / a0,
            a2: (1.0 - alpha) / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    fn filter(&mut self, sample: f64) -> f64 {
        let filtered = self.b0 * sample + self.s1;
        self.s1 = self.b1 * sample - self.a1 * filtered + self.s2;
        self.s2 = self.b0 * sample - self.a2 * filtered;
        filtered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain(cutoff: f64, hz: f64) -> f64 {
        let rate = 48_000.0;
        let mut filter = Lowpass::new(cutoff, rate).unwrap();
        (0..48_000)
            .map(|index| filter.filter((TAU * hz * f64::from(index) / rate).sin()))
            .skip(24_000)
            .fold(0.0, |peak, sample| sample.abs().max(peak))
    }

    #[test]
    fn the_filter_is_flat_below_its_cutoff_and_falls_48_db_an_octave_above() {
        for (hz, low, high) in [
            (150.0, 0.99, 1.001),
            (300.0, 0.70, 0.71),
            (600.0, 0.0035, 0.0045),
        ] {
            let gain = gain(300.0, hz);
            assert!((low..high).contains(&gain), "{hz} Hz passed at {gain}");
        }
    }

    #[test]
    fn a_cutoff_must_lie_below_half_the_rate() {
        assert!(Lowpass::new(300.0, 600.0).is_none());
        assert!(Lowpass::new(0.0, 48_000.0).is_none());
        assert!(Lowpass::new(300.0, 601.0).is_some());
    }
}
