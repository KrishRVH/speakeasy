use anyhow::Context as _;

use super::*;
use crate::movie::tests::frame;

#[derive(Default)]
struct Tape {
    shades: Vec<u8>,
    checkpoints: Vec<Duration>,
}

impl Codec for Tape {
    #[cfg(not(target_os = "windows"))]
    const CLOCK: Clock = Clock::Nanoseconds;
    #[cfg(target_os = "windows")]
    const CLOCK: Clock = Clock::HundredNanoseconds;

    fn open(_: &Path, _: [u32; 2], _: u64) -> anyhow::Result<Self> {
        Ok(Self::default())
    }

    fn write(
        &mut self,
        pixels: &[u8],
        _: u32,
        _: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<bool> {
        self.shades.extend(pixels.first());
        Ok(true)
    }

    fn checkpoint(&mut self, at: Duration) -> anyhow::Result<()> {
        self.checkpoints.push(at);
        Ok(())
    }

    fn finish(self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Layouts {
    acknowledged: Vec<(u32, [u32; 4])>,
    accepts: bool,
}

impl Observer for Layouts {
    fn cancelled(&mut self) -> bool {
        false
    }

    fn geometry(&mut self, sample: u32, content: [u32; 4], _: crate::Part) -> anyhow::Result<bool> {
        self.acknowledged.push((sample, content));
        Ok(self.accepts)
    }
}

fn open(edge: u32) -> anyhow::Result<Encoder<Tape>> {
    Encoder::open(Path::new("synthetic.mp4"), Canvas::new([edge, edge])?, 0)
}

#[test]
fn each_tick_presents_the_newest_capture_at_or_before_it() -> anyhow::Result<()> {
    let mut encoder = open(2)?;
    assert!(
        encoder
            .write(&frame(2, 2, 10), Duration::ZERO, &mut || false)?
            .is_continue()
    );
    assert!(
        encoder
            .write(&frame(2, 2, 20), Duration::from_millis(100), &mut || false)?
            .is_continue()
    );
    assert_eq!(
        encoder.codec.shades,
        [10, 10, 10, 20],
        "A tick presented a capture from its future"
    );
    let ticks: Vec<Duration> = (0..4).map(|index| Tape::CLOCK.time(index)).collect();
    assert_eq!(encoder.codec.checkpoints, ticks);
    let coverage = encoder.coverage().context("No sample was written")?;
    assert_eq!(
        (coverage.last, coverage.at, coverage.frames),
        (Duration::from_millis(100), ticks[3], 4)
    );
    encoder.finish()
}

#[test]
fn a_layout_is_acknowledged_before_the_first_sample_that_shows_it() -> anyhow::Result<()> {
    let mut encoder = open(4)?;
    let mut layouts = Layouts {
        acknowledged: Vec::new(),
        accepts: true,
    };
    assert!(
        encoder
            .write(&frame(4, 4, 10), Duration::ZERO, &mut layouts)?
            .is_continue()
    );
    assert!(
        encoder
            .write(&frame(4, 2, 20), Duration::from_millis(100), &mut layouts)?
            .is_continue()
    );
    assert_eq!(layouts.acknowledged, [(0, [0, 0, 4, 4]), (3, [0, 1, 4, 2])]);
    assert_eq!(encoder.codec.shades.len(), 4);
    encoder.finish()
}

#[test]
fn a_refused_layout_is_never_presented() -> anyhow::Result<()> {
    let mut encoder = open(4)?;
    let mut refused = Layouts {
        acknowledged: Vec::new(),
        accepts: false,
    };
    assert!(
        encoder
            .write(&frame(4, 4, 10), Duration::ZERO, &mut refused)?
            .is_break()
    );
    assert!(
        encoder.codec.shades.is_empty(),
        "A sample was written under a refused layout"
    );
    assert!(encoder.coverage().is_none());
    Ok(())
}

#[test]
fn cancellation_ends_the_timeline_with_its_committed_coverage() -> anyhow::Result<()> {
    let mut encoder = open(2)?;
    assert!(
        encoder
            .write(&frame(2, 2, 10), Duration::ZERO, &mut || false)?
            .is_continue()
    );
    assert!(
        encoder
            .write(&frame(2, 2, 20), Duration::from_millis(100), &mut || true)?
            .is_break()
    );
    assert_eq!(
        encoder.codec.shades,
        [10],
        "A cancelled timeline kept writing"
    );
    assert_eq!(encoder.coverage().map(|coverage| coverage.frames), Some(1));
    Ok(())
}

#[test]
fn a_capture_too_far_ahead_fails_instead_of_backfilling_it() -> anyhow::Result<()> {
    let mut encoder = open(2)?;
    assert!(
        encoder
            .write(&frame(2, 2, 10), Duration::ZERO, &mut || false)?
            .is_continue()
    );
    let behind = encoder
        .write(&frame(2, 2, 20), Duration::from_secs(3), &mut || false)
        .err()
        .context("A three-second gap was backfilled")?;
    assert_eq!(
        behind.to_string(),
        "Video encoding fell behind; retained frames remain available"
    );
    assert_eq!(encoder.codec.shades, [10; 61]);
    Ok(())
}
