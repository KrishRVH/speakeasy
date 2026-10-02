//! When one session reached each stage, for the opt-in timing report: durations and counts only,
//! never audio or text.

use std::time::{Duration, Instant};

/// Stage instants on the owner's clock, so every span shares one clock domain.
#[derive(Clone, Copy, Debug)]
pub(super) struct Timeline {
    pressed: Instant,
    /// How long the capture thread took to open and start the device.
    device: Option<Duration>,
    audio: Option<Instant>,
    released: Option<Instant>,
    sealed: Option<Instant>,
    recorded: Option<Duration>,
    transcribing: Option<Instant>,
    transcribed: Option<Instant>,
    /// Whether the text came from recognizing a pause with identical audio.
    speculated: bool,
}

impl Timeline {
    pub(super) const fn new(pressed: Instant) -> Self {
        Self {
            pressed,
            device: None,
            audio: None,
            released: None,
            sealed: None,
            recorded: None,
            transcribing: None,
            transcribed: None,
            speculated: false,
        }
    }

    pub(super) fn audio(&mut self, at: Instant, device: Duration) {
        self.audio.get_or_insert(at);
        self.device.get_or_insert(device);
    }

    pub(super) fn released(&mut self, at: Instant) {
        self.released.get_or_insert(at);
    }

    /// Capture handed over `recorded` of audio after trimming.
    pub(super) fn sealed(&mut self, at: Instant, recorded: Duration) {
        self.sealed.get_or_insert(at);
        self.recorded.get_or_insert(recorded);
    }

    pub(super) fn transcribing(&mut self, at: Instant) {
        self.transcribing.get_or_insert(at);
    }

    pub(super) fn transcribed(&mut self, at: Instant) {
        self.transcribed.get_or_insert(at);
    }

    /// The text came from a pause's recognition, available at `at` without a request of its own.
    pub(super) fn speculated(&mut self, at: Instant) {
        self.speculated = true;
        self.transcribing.get_or_insert(at);
        self.transcribed.get_or_insert(at);
    }

    /// One line of the spans this session reached, ending at `ended` with `outcome`.
    pub(super) fn report(&self, ended: Instant, outcome: &str) -> String {
        let spans = [
            ("press→audio", Some(self.pressed), self.audio),
            ("release→sealed", self.released, self.sealed),
            ("sealed→engine", self.sealed, self.transcribing),
            ("engine", self.transcribing, self.transcribed),
            ("text→done", self.transcribed, Some(ended)),
            ("release→done", self.released, Some(ended)),
        ];
        let spans = spans.into_iter().filter_map(|(label, from, to)| {
            Some(milliseconds(label, to?.saturating_duration_since(from?)))
        });
        let device = self.device.map(|device| milliseconds("device", device));
        let audio = self
            .recorded
            .map(|recorded| format!("audio {:.2} s", recorded.as_secs_f64()));
        let speculated = self.speculated.then(|| "speculated".to_owned());
        let parts: Vec<String> = spans
            .chain(device)
            .chain(audio)
            .chain(speculated)
            .chain([outcome.to_owned()])
            .collect();
        format!("speakeasy timing: {}", parts.join(" · "))
    }
}

fn milliseconds(label: &str, duration: Duration) -> String {
    format!("{label} {:.1} ms", duration.as_secs_f64() * 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_only_the_spans_a_session_reached() {
        let pressed = Instant::now();
        let at = |milliseconds| pressed + Duration::from_millis(milliseconds);
        let mut timeline = Timeline::new(pressed);
        timeline.audio(at(40), Duration::from_millis(30));
        timeline.released(at(1_000));
        timeline.sealed(at(1_002), Duration::from_millis(900));
        timeline.transcribing(at(1_002));
        timeline.transcribed(at(1_150));
        assert_eq!(
            timeline.report(at(1_160), "done"),
            "speakeasy timing: press→audio 40.0 ms · release→sealed 2.0 ms · sealed→engine 0.0 ms \
             · engine 148.0 ms · text→done 10.0 ms · release→done 160.0 ms · device 30.0 ms \
             · audio 0.90 s · done"
        );
        let cancelled = Timeline::new(pressed);
        assert_eq!(
            cancelled.report(at(5), "cancelled"),
            "speakeasy timing: cancelled"
        );
    }
}
