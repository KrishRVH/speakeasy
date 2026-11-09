#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) mod geometry;

use std::{
    collections::HashMap,
    fmt, thread,
    time::{Duration, Instant},
};

use async_channel::Sender;

#[cfg(target_os = "linux")]
use self::unsupported as native;
#[cfg(target_os = "macos")]
use crate::macos::screen as native;
#[cfg(target_os = "windows")]
use crate::windows::screen as native;
use crate::{NativeWindow, StoppableThread};

const POINTER_INTERVAL: Duration = Duration::from_millis(8);

const WINDOW_STEP: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Screen,
    Window,
    Area,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Part {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Part {
    pub const WHOLE: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    #[must_use]
    pub fn between([x, y]: [f32; 2], [other_x, other_y]: [f32; 2]) -> Self {
        let clip = |value: f32| value.clamp(0.0, 1.0);
        Self {
            left: clip(x.min(other_x)),
            top: clip(y.min(other_y)),
            right: clip(x.max(other_x)),
            bottom: clip(y.max(other_y)),
        }
    }

    #[must_use]
    pub fn width(self) -> f32 {
        self.right - self.left
    }

    #[must_use]
    pub fn height(self) -> f32 {
        self.bottom - self.top
    }

    #[must_use]
    pub fn within(self, [x, y]: [f32; 2]) -> [f32; 2] {
        let normalized = |value: f32, start: f32, extent: f32| {
            if extent > 0.0 {
                (value - start) / extent
            } else {
                value
            }
        };
        [
            normalized(x, self.left, self.width()),
            normalized(y, self.top, self.height()),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub(crate) display: Display,
    pub(crate) region: Region,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Region {
    Whole,
    Window(native::Window, Part),
    Area(Part),
}

impl Target {
    pub fn screen_under_pointer() -> anyhow::Result<Self> {
        Ok(Self {
            display: Display::under_pointer()?,
            region: Region::Whole,
        })
    }

    pub fn window_under_pointer() -> anyhow::Result<Option<Self>> {
        let display = Display::under_pointer()?;
        Ok(
            native::window_under_pointer(display.native)?.map(|(window, part)| Self {
                display,
                region: Region::Window(window, part),
            }),
        )
    }

    #[must_use]
    pub const fn area(display: Display, part: Part) -> Self {
        Self {
            display,
            region: Region::Area(part),
        }
    }

    #[must_use]
    pub const fn display(&self) -> Display {
        self.display
    }

    #[must_use]
    pub const fn part(&self) -> Part {
        match self.region {
            Region::Whole => Part::WHOLE,
            Region::Window(_, part) | Region::Area(part) => part,
        }
    }
}

pub struct Frame {
    pub at: Instant,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub part: Part,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            at,
            width,
            height,
            bgra: _,
            part,
        } = self;
        f.debug_struct("Frame")
            .field("at", at)
            .field("width", width)
            .field("height", height)
            .field("part", part)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum ScreenEvent {
    Frame(Frame),
    WindowGone(WindowGone),
    Failed(String),
    MovieStopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowGone {
    Closed,
    OffScreen,
}

impl fmt::Display for WindowGone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "The recorded window closed.",
            Self::OffScreen => "The recorded window went off screen.",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub(crate) native: native::Display,
    pub size: [f32; 2],
}

impl Display {
    pub fn under_pointer() -> anyhow::Result<Self> {
        native::display_under_pointer()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerSample {
    Observed {
        at: Instant,
        position: [f32; 2],
        part: Part,
        drawing: bool,
        pressed: bool,
    },
    Gap(Instant),
}

impl PointerSample {
    #[must_use]
    pub const fn at(self) -> Instant {
        match self {
            Self::Observed { at, .. } | Self::Gap(at) => at,
        }
    }

    fn differs(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Observed {
                    position,
                    part,
                    drawing,
                    pressed,
                    ..
                },
                Self::Observed {
                    position: other_position,
                    part: other_part,
                    drawing: other_drawing,
                    pressed: other_pressed,
                    ..
                },
            ) => {
                (position, part, drawing, pressed)
                    != (other_position, other_part, other_drawing, other_pressed)
            },
            (Self::Gap(_), Self::Gap(_)) => false,
            _ => true,
        }
    }
}

#[derive(Debug)]
pub struct ScreenCapture {
    thread: StoppableThread,
}

impl ScreenCapture {
    pub fn start(
        target: Target,
        interval: Duration,
        longest_edge: u32,
        events: Sender<ScreenEvent>,
        movie: Option<crate::MovieTarget>,
    ) -> anyhow::Result<Self> {
        let thread = StoppableThread::spawn("screen-capture", move |stopping| {
            native::capture(target, interval, longest_edge, &events, &stopping, movie);
        })?;
        Ok(Self { thread })
    }

    pub fn request_stop(&self) {
        self.thread.request_stop();
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.stopped()
    }
}

#[derive(Debug)]
pub struct PointerMonitor {
    thread: StoppableThread,
}

impl PointerMonitor {
    pub fn start(
        target: Target,
        samples: Sender<PointerSample>,
        shortcut: crate::MacShortcut,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            samples.capacity().is_some_and(|capacity| capacity >= 2),
            "Pointer sampling needs a bounded lane with at least two slots"
        );
        let thread = StoppableThread::spawn("pointer", move |stopping| {
            let mut pending = Samples {
                outgoing: samples,
                last: None,
            };
            while !stopping.is_closed() && !pending.outgoing.is_closed() {
                pending.send(native::pointer(target, shortcut), Instant::now());
                thread::sleep(POINTER_INTERVAL);
            }
        })?;
        Ok(Self { thread })
    }

    pub fn request_stop(&self) {
        self.thread.request_stop();
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.stopped()
    }
}

struct Samples {
    outgoing: Sender<PointerSample>,
    last: Option<PointerSample>,
}

impl Samples {
    fn send(&mut self, sample: Option<PointerSample>, now: Instant) {
        let mut sample = sample.unwrap_or(PointerSample::Gap(now));
        if self.last.is_some_and(|last| !last.differs(&sample)) {
            return;
        }

        if matches!(sample, PointerSample::Observed { .. })
            && self
                .outgoing
                .capacity()
                .is_some_and(|capacity| self.outgoing.len().saturating_add(1) >= capacity)
        {
            sample = PointerSample::Gap(sample.at());
        }
        if self.last.is_none_or(|last| last.differs(&sample))
            && self.outgoing.try_send(sample).is_ok()
        {
            self.last = Some(sample);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowLayer {
    Normal,
    Floating,
    Menu,
    Bar,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedWindow {
    pub id: u32,
    pub app: u32,
    pub layer: WindowLayer,
    pub part: Part,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowLayout {
    pub at: Instant,
    pub windows: Vec<PlacedWindow>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Listed {
    pub(crate) window: u64,
    pub(crate) process: u64,
    pub(crate) layer: WindowLayer,
    pub(crate) part: Part,
}

#[derive(Debug)]
pub struct WindowWatch {
    thread: StoppableThread,
}

impl WindowWatch {
    pub fn start(
        display: Display,
        interval: Duration,
        layouts: Sender<WindowLayout>,
    ) -> anyhow::Result<Self> {
        let thread = StoppableThread::spawn("windows", move |stopping| {
            let mut watcher = Watcher::default();
            let mut due = Instant::now();
            while !stopping.is_closed() && !layouts.is_closed() {
                let now = Instant::now();
                if now >= due {
                    due = now.checked_add(interval).unwrap_or(now);
                    watcher.deliver(native::stack(display.native), now, &layouts);
                }
                thread::sleep(WINDOW_STEP);
            }
        })?;
        Ok(Self { thread })
    }

    pub fn request_stop(&self) {
        self.thread.request_stop();
    }

    pub fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.stopped()
    }
}

#[derive(Default)]
struct Watcher {
    windows: HashMap<u64, u32>,
    apps: HashMap<u64, u32>,
    sent: Option<Vec<PlacedWindow>>,
}

impl Watcher {
    fn deliver(
        &mut self,
        listed: Option<Vec<Listed>>,
        at: Instant,
        layouts: &Sender<WindowLayout>,
    ) {
        let Some(layout) = listed.and_then(|listed| self.observe(at, listed)) else {
            return;
        };
        if layouts.try_send(layout.clone()).is_ok() {
            self.sent = Some(layout.windows);
        }
    }

    fn observe(&mut self, at: Instant, listed: Vec<Listed>) -> Option<WindowLayout> {
        let windows: Vec<PlacedWindow> = listed
            .into_iter()
            .map(|listed| PlacedWindow {
                id: opaque(&mut self.windows, listed.window),
                app: opaque(&mut self.apps, listed.process),
                layer: listed.layer,
                part: listed.part,
            })
            .collect();
        (self.sent.as_ref() != Some(&windows)).then_some(WindowLayout { at, windows })
    }
}

fn opaque(names: &mut HashMap<u64, u32>, native: u64) -> u32 {
    let next = u32::try_from(names.len()).unwrap_or(u32::MAX);
    *names.entry(native).or_insert(next)
}

impl NativeWindow {
    pub fn configure_overlay(&self, display: Display) -> anyhow::Result<()> {
        native::configure_overlay(self, display.native)
    }

    pub fn set_overlay_visible(&self, visible: bool) {
        native::set_overlay_visible(self, visible);
    }

    pub fn set_overlay_input(&self, takes_input: bool) {
        native::set_overlay_input(self, takes_input);
    }
}

#[cfg(target_os = "linux")]
mod unsupported {
    use std::{convert::Infallible, time::Duration};

    use anyhow::bail;
    use async_channel::{Receiver, Sender};

    use super::{Listed, Part, PointerSample, ScreenEvent, Target};
    use crate::{MovieTarget, NativeWindow, linux::SHOWS_UNSUPPORTED};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Display;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Window;

    pub(super) fn display_under_pointer() -> anyhow::Result<super::Display> {
        bail!(SHOWS_UNSUPPORTED)
    }

    pub(super) fn window_under_pointer(_: Display) -> anyhow::Result<Option<(Window, Part)>> {
        bail!(SHOWS_UNSUPPORTED)
    }

    pub(super) fn capture(
        _: Target,
        _: Duration,
        _: u32,
        _: &Sender<ScreenEvent>,
        _: &Receiver<Infallible>,
        _: Option<MovieTarget>,
    ) {
    }

    pub(super) const fn pointer(_: Target, _: crate::MacShortcut) -> Option<PointerSample> {
        None
    }

    pub(super) const fn stack(_: Display) -> Option<Vec<Listed>> {
        None
    }

    pub(super) fn configure_overlay(_: &NativeWindow, _: Display) -> anyhow::Result<()> {
        bail!(SHOWS_UNSUPPORTED)
    }

    pub(super) const fn set_overlay_visible(_: &NativeWindow, _: bool) {}

    pub(super) const fn set_overlay_input(_: &NativeWindow, _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_gaps_survive_backpressure_and_stationary_resizes_are_observed() {
        let started = Instant::now();
        let at = |ms| started.checked_add(Duration::from_millis(ms)).unwrap();
        let observed = |ms, part| PointerSample::Observed {
            at: at(ms),
            position: [0.5, 0.5],
            part,
            drawing: false,
            pressed: true,
        };
        let (samples, received) = async_channel::bounded(2);
        let mut pending = Samples {
            outgoing: samples,
            last: None,
        };
        pending.send(Some(observed(0, Part::WHOLE)), at(0));
        pending.send(None, at(10));
        let resized = Part::between([0.25, 0.25], [0.75, 0.75]);
        pending.send(Some(observed(20, resized)), at(20));
        assert_eq!(received.try_recv().unwrap(), observed(0, Part::WHOLE));
        pending.send(Some(observed(30, resized)), at(30));
        assert_eq!(received.try_recv().unwrap(), PointerSample::Gap(at(10)));
        pending.send(Some(observed(40, resized)), at(40));
        assert_eq!(received.try_recv().unwrap(), observed(40, resized));
        pending.send(Some(observed(50, resized)), at(50));
        assert!(received.is_empty());
        pending.send(Some(observed(60, Part::WHOLE)), at(60));
        assert_eq!(received.try_recv().unwrap(), observed(60, Part::WHOLE));
    }

    #[test]
    fn a_lost_button_transition_leaves_a_gap_when_the_producer_stops() {
        let start = Instant::now();
        let sample = |pressed| PointerSample::Observed {
            at: start,
            position: [0.5, 0.5],
            part: Part::WHOLE,
            drawing: false,
            pressed,
        };
        let (outgoing, incoming) = async_channel::bounded(2);
        let mut pending = Samples {
            outgoing,
            last: None,
        };
        pending.send(Some(sample(true)), start);
        pending.send(Some(sample(false)), start);
        pending.send(Some(sample(true)), start);
        drop(pending);
        assert_eq!(incoming.try_recv().unwrap(), sample(true));
        assert_eq!(incoming.try_recv().unwrap(), PointerSample::Gap(start));
        assert!(incoming.is_closed());
    }

    #[test]
    fn points_map_from_a_display_into_its_part() {
        let part = Part::between([0.75, 0.5], [0.25, -0.5]);
        assert_eq!(
            part,
            Part {
                left: 0.25,
                top: 0.0,
                right: 0.75,
                bottom: 0.5
            }
        );
        assert_eq!(part.within([0.5, 0.25]), [0.5, 0.5]);
        assert_eq!(part.within([1.0, 0.75]), [1.5, 1.5], "Outside the part");
        let flat = Part::between([0.0, 0.5], [1.0, 0.5]);
        assert_eq!(
            flat.within([0.3, 0.7]),
            [0.3, 0.7],
            "An empty extent maps nothing"
        );
    }

    #[test]
    fn debugging_a_frame_never_prints_its_pixels() {
        let at = Instant::now();
        let shown = |shade: u8| {
            let frame = Frame {
                at,
                width: 2,
                height: 2,
                bgra: vec![shade; 16],
                part: Part::WHOLE,
            };
            format!("{frame:?}")
        };
        assert_eq!(shown(0x00), shown(0xFF));
    }

    #[test]
    fn window_layouts_carry_opaque_names_and_only_their_changes() {
        let listed = |window, process, left| Listed {
            window,
            process,
            layer: WindowLayer::Normal,
            part: Part::between([left, 0.1], [left + 0.3, 0.6]),
        };
        let (lane, layouts) = async_channel::bounded(1);
        let mut watcher = Watcher::default();
        let at = Instant::now();
        let first = vec![listed(0x5E2, 4_242, 0.1), listed(0x7A0, 4_242, 0.5)];
        watcher.deliver(Some(first.clone()), at, &lane);
        let shown = layouts.try_recv().unwrap();
        let names: Vec<(u32, u32)> = shown
            .windows
            .iter()
            .map(|window| (window.id, window.app))
            .collect();
        assert_eq!(names, [(0, 0), (1, 0)]);
        watcher.deliver(Some(first), at, &lane);
        assert!(
            layouts.try_recv().is_err(),
            "An unchanged layout is sent once"
        );

        let raised = vec![
            listed(0x7A0, 4_242, 0.5),
            listed(0x900, 77, 0.2),
            listed(0x5E2, 4_242, 0.1),
        ];
        lane.try_send(shown).unwrap();
        watcher.deliver(Some(raised.clone()), at, &lane);
        assert_eq!(
            layouts.try_recv().unwrap().windows.len(),
            2,
            "A full lane drops the change"
        );
        watcher.deliver(Some(raised), at, &lane);
        let names: Vec<(u32, u32)> = layouts
            .try_recv()
            .unwrap()
            .windows
            .iter()
            .map(|window| (window.id, window.app))
            .collect();
        assert_eq!(
            names,
            [(1, 0), (2, 1), (0, 0)],
            "and it arrives once there is room"
        );
    }
}
