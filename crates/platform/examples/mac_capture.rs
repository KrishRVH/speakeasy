//! Explicit owned-window screen acceptance. It neither saves pixels nor accesses audio or input hooks.

#[cfg(target_os = "macos")]
#[path = "mac_capture/pattern.rs"]
mod pattern;

#[cfg(not(target_os = "macos"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("This owned-window check requires macOS")
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    native::run()
}

#[cfg(target_os = "macos")]
mod native {
    use std::{
        io::{BufRead as _, BufReader, Write as _},
        process::{Child, Command, Stdio},
        sync::mpsc,
        time::{Duration, Instant},
    };

    use anyhow::{Context as _, ensure};
    use objc2::{MainThreadMarker, MainThreadOnly as _, rc::Retained};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSWindow,
        NSWindowOrderingMode, NSWindowStyleMask,
    };
    use objc2_core_graphics::{CGDisplayBounds, CGError, CGGetActiveDisplayList, CGMainDisplayID};
    use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};
    use speakeasy_platform::{Display, Part, ScreenCapture, ScreenEvent, Target};

    pub(super) fn run() -> anyhow::Result<()> {
        let mode = std::env::args()
            .nth(1)
            .context("Use --owned-screen to opt into synthetic window capture")?;
        ensure!(
            matches!(
                mode.as_str(),
                "--owned-screen" | "--source" | "--owned-window" | "--window-source"
            ),
            "Use --owned-screen to opt into synthetic window capture"
        );
        let main = MainThreadMarker::new()
            .context("Native window acceptance must start on the main thread")?;
        let app = NSApplication::sharedApplication(main);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        if mode == "--source" || mode == "--window-source" {
            return source(main, mode == "--window-source");
        }
        let moving = mode == "--owned-window";
        let mut source = Source(
            Command::new(std::env::current_exe()?)
                .arg(if moving {
                    "--window-source"
                } else {
                    "--source"
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let mut ready = String::new();
        BufReader::new(
            source
                .0
                .stdout
                .take()
                .context("Synthetic source has no readiness lane")?,
        )
        .read_line(&mut ready)?;
        let number: u32 = ready
            .trim()
            .strip_prefix("ready ")
            .context("Synthetic window did not become ready")?
            .parse()?;
        let mut displays = [0; 2];
        let mut count = 0;
        // SAFETY: the two-element display array and count output are writable for this query.
        let status = unsafe { CGGetActiveDisplayList(2, displays.as_mut_ptr(), &raw mut count) };
        ensure!(
            status == CGError::Success && count == 1 && displays[0] == CGMainDisplayID(),
            "This owned-window check requires exactly one active display"
        );
        let bounds = CGDisplayBounds(CGMainDisplayID());
        let display = Display::under_pointer()?;
        let target = if moving {
            Some(owned_target(display, number, source.0.id())?)
        } else {
            None
        };
        ensure!(
            (f64::from(display.size[0]) - bounds.size.width).abs() < 0.01
                && (f64::from(display.size[1]) - bounds.size.height).abs() < 0.01
                && display.size[0] >= 600.0
                && display.size[1] >= 450.0
                && bounds.origin.x == 0.0
                && bounds.origin.y == 0.0,
            "Use the main display for this owned-window check"
        );
        let overlay = window(main, [160.0, 150.0, 100.0, 100.0], 101)?;
        overlay.setBackgroundColor(Some(&color([20, 20, 230])));
        overlay.orderFrontRegardless();
        overlay.display();
        let top = f64::from(display.size[1]) - 390.0;
        let part = Part::between(
            [ratio(110.0, display.size[0]), ratio(top, display.size[1])],
            [
                ratio(490.0, display.size[0]),
                ratio(top + 280.0, display.size[1]),
            ],
        );
        let (sent, reply) = mpsc::sync_channel(1);
        let worker = speakeasy_platform::OwnedThread::spawn(
            "owned Mac capture acceptance",
            move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .map_err(anyhow::Error::from)
                    .and_then(|runtime| runtime.block_on(verify(display, part, target)));
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "The main-thread owner waits for this terminal result while pumping its native run loop"
                )]
                let _ = sent.send(result);
            },
        )?;
        let result = loop {
            match reply.try_recv() {
                Ok(result) => break result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("Native capture worker ended without a result")
                },
                Err(mpsc::TryRecvError::Empty) => pump(),
            }
        };
        drop(worker);
        overlay.close();
        result?;
        writeln!(
            std::io::stdout().lock(),
            "Owned Mac capture passed: changing pixels, exclusion, timestamps, stop and restart"
        )?;
        Ok(())
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "Owned fixture coordinates are bounded by the checked display before normalization"
    )]
    fn ratio(value: f64, size: f32) -> f32 {
        (value / f64::from(size)) as f32
    }

    fn source(main: MainThreadMarker, moving: bool) -> anyhow::Result<()> {
        let display = CGDisplayBounds(CGMainDisplayID());
        let initial = if moving {
            [0.0, 0.0, display.size.width, display.size.height]
        } else {
            [100.0, 100.0, 400.0, 300.0]
        };
        let window = window(main, initial, if moving { 0 } else { 100 })?;
        if moving {
            super::pattern::attach(&window, main)?;
        }
        let popup = if moving {
            let popup = self::window(main, [160.0, 150.0, 80.0, 60.0], 1)?;
            popup.setBackgroundColor(Some(&color([220, 220, 20])));
            // SAFETY: both owned windows stay on the main thread, and the parent retains its child.
            unsafe {
                window.addChildWindow_ordered(&popup, NSWindowOrderingMode::Above);
            }
            popup.orderOut(None);
            Some(popup)
        } else {
            None
        };
        window.setBackgroundColor(Some(&color([230, 30, 20])));
        window.orderFrontRegardless();
        window.display();
        if moving {
            NSApplication::sharedApplication(main).activate();
        }
        for _ in 0..5 {
            pump();
        }
        writeln!(std::io::stdout().lock(), "ready {}", window.windowNumber())?;
        std::io::stdout().lock().flush()?;
        let started = Instant::now();
        let mut previous = 0;
        let mut menu_visible = false;
        loop {
            let elapsed = started.elapsed().as_millis();
            let phase = if moving { elapsed / 400 } else { elapsed / 200 } % 2;
            if phase != previous {
                if moving && started.elapsed() >= Duration::from_secs(2) {
                    move_window(&window, popup.as_deref(), phase);
                }
                window.setBackgroundColor(Some(&color(if phase == 0 {
                    [230, 30, 20]
                } else {
                    [20, 220, 30]
                })));
                if let Some(view) = window.contentView() {
                    view.setNeedsDisplay(true);
                }
                window.display();
                previous = phase;
            }
            if let Some(popup) = &popup {
                let visible = started.elapsed() >= Duration::from_secs(2)
                    && (600..700).contains(&(started.elapsed().as_millis() % 1600));
                if visible != menu_visible {
                    show_menu(popup, visible);
                    menu_visible = visible;
                }
            }
            pump();
        }
    }

    fn move_window(window: &NSWindow, popup: Option<&NSWindow>, phase: u128) {
        let [x, y, width, height] = if phase == 0 {
            [100.0, 100.0, 400.0, 300.0]
        } else {
            [220.0, 140.0, 320.0, 240.0]
        };
        window.setFrame_display(
            NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)),
            true,
        );
        if let Some(popup) = popup {
            popup.setFrameOrigin(NSPoint::new(x + width * 0.4, y + height * 0.4));
        }
    }

    #[cfg(feature = "native-acceptance")]
    fn owned_target(display: Display, number: u32, process: u32) -> anyhow::Result<Target> {
        Target::owned_mac_window(display, number, process)
    }

    #[cfg(not(feature = "native-acceptance"))]
    fn owned_target(_: Display, _: u32, _: u32) -> anyhow::Result<Target> {
        anyhow::bail!("Build with --features native-acceptance for owned-window selection")
    }

    fn show_menu(popup: &NSWindow, visible: bool) {
        if visible {
            popup.orderFrontRegardless();
            popup.display();
        } else {
            popup.orderOut(None);
        }
    }

    fn window(
        main: MainThreadMarker,
        [x, y, width, height]: [f64; 4],
        level: isize,
    ) -> anyhow::Result<Retained<NSWindow>> {
        // SAFETY: allocation and all window work stay on the main thread; the window owns a
        // fixed synthetic rectangle and is never released by Cocoa while Rust retains it.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main),
                NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust retains this window through close, so Cocoa must not independently release it.
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setHasShadow(false);
        window.setOpaque(true);
        window.setLevel(level);
        ensure!(
            window.windowNumber() > 0,
            "Synthetic window has no native identity"
        );
        Ok(window)
    }

    fn color([red, green, blue]: [u8; 3]) -> Retained<NSColor> {
        NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(red) / 255.0,
            f64::from(green) / 255.0,
            f64::from(blue) / 255.0,
            1.0,
        )
    }

    fn pump() {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.02));
    }

    async fn verify(display: Display, part: Part, target: Option<Target>) -> anyhow::Result<()> {
        for _ in 0..2 {
            let (events, frames) = async_channel::bounded(3);
            let capture = ScreenCapture::start(
                target.unwrap_or_else(|| Target::area(display, part)),
                Duration::from_millis(33),
                2560,
                events,
                None,
            )?;
            let result = inspect(&frames, part, target.map(|_| display)).await;
            capture.request_stop();
            capture.stopped().await;
            while frames.try_recv().is_ok() {}
            ensure!(
                frames.is_closed(),
                "Capture retained its event lane after stop"
            );
            result?;
        }
        Ok(())
    }

    async fn inspect(
        frames: &async_channel::Receiver<ScreenEvent>,
        part: Part,
        moving: Option<Display>,
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut seen = [false; 2];
        let mut previous = None;
        let mut count = 0_u32;
        let mut moved = false;
        let mut previous_part = None;
        let mut saw_menu = false;
        loop {
            let event = tokio::select! {
                () = &mut deadline => break,
                event = frames.recv() => event?,
            };
            let ScreenEvent::Frame(frame) = event else {
                anyhow::bail!(
                    "Owned native screen capture failed; check Screen Recording permission"
                );
            };
            ensure!(
                (moving.is_some() || frame.part == part) && frame.width > 0 && frame.height > 0,
                "Owned capture changed its fixed region"
            );
            ensure!(
                frame.at <= Instant::now() && previous.is_none_or(|last| frame.at > last),
                "Native timestamps are not monotonic presentation times"
            );
            previous = Some(frame.at);
            if let Some(display) = moving {
                let valid = [
                    [
                        0.0,
                        0.0,
                        f64::from(display.size[0]),
                        f64::from(display.size[1]),
                    ],
                    [100.0, 100.0, 400.0, 300.0],
                    [220.0, 140.0, 320.0, 240.0],
                ]
                .into_iter()
                .any(|[x, y, width, height]| {
                    let top = f64::from(display.size[1]) - y - height;
                    let expected = Part::between(
                        [ratio(x, display.size[0]), ratio(top, display.size[1])],
                        [
                            ratio(x + width, display.size[0]),
                            ratio(top + height, display.size[1]),
                        ],
                    );
                    [
                        frame.part.left - expected.left,
                        frame.part.top - expected.top,
                        frame.part.right - expected.right,
                        frame.part.bottom - expected.bottom,
                    ]
                    .into_iter()
                    .all(|delta| delta.abs() < 0.001)
                });
                ensure!(
                    valid,
                    "Native frame has unexpected owned-window geometry: {:?}",
                    frame.part
                );
                moved |= previous_part.is_some_and(|part| part != frame.part);
                previous_part = Some(frame.part);
            }
            let patterns = [[20, 30, 230], [30, 220, 20]];
            if let Some(display) = moving {
                landmarks(&frame, display)?;
                for pixel in frame.bgra.as_chunks::<4>().0 {
                    let matches = |pattern| matches_pixel(*pixel, pattern);
                    let menu = matches([20, 220, 220]);
                    saw_menu |= menu;
                    ensure!(
                        !matches([230, 20, 20]) && pixel[..3].iter().any(|channel| *channel > 10),
                        "The window stream contains pixels outside its owned source or includes the excluded window"
                    );
                }
                for (present, pattern) in seen.iter_mut().zip(patterns) {
                    *present |= frame
                        .bgra
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|pixel| matches_pixel(*pixel, pattern));
                }
                count = count.saturating_add(1);
                continue;
            }
            let matched = patterns.iter().position(|pattern| {
                frame.bgra.as_chunks::<4>().0.iter().all(|pixel| {
                    pixel[..3]
                        .iter()
                        .zip(pattern)
                        .all(|(actual, expected)| actual.abs_diff(*expected) <= 3)
                })
            });
            let index = matched.context("The owned capture contains pixels outside its synthetic source or includes the excluded window")?;
            *seen
                .get_mut(index)
                .context("Synthetic pattern index is invalid")? = true;
            count = count.saturating_add(1);
        }
        ensure!(
            count >= 4 && seen.into_iter().all(|present| present),
            "The owned capture missed changing content: {count} frames, observed colors {seen:?}"
        );
        ensure!(
            moving.is_none() || moved,
            "The window stream did not follow movement and resizing"
        );
        ensure!(
            moving.is_none() || saw_menu,
            "The window stream missed the brief owned child window"
        );
        Ok(())
    }

    struct Source(Child);

    fn matches_pixel(pixel: [u8; 4], pattern: [u8; 3]) -> bool {
        pixel[..3]
            .iter()
            .zip(pattern)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 3)
    }

    fn landmarks(frame: &speakeasy_platform::Frame, display: Display) -> anyhow::Result<()> {
        let width = frame.part.width() * display.size[0];
        let identity = if width > 600.0 {
            [240, 240, 240]
        } else if width > 350.0 {
            [180, 180, 240]
        } else {
            [180, 240, 180]
        };
        for ([x, y], expected) in [([20, 75], identity), ([78, 30], [220, 20, 220])] {
            let x = frame.width.saturating_mul(x) / 100;
            let y = frame.height.saturating_mul(y) / 100;
            let offset = usize::try_from(
                u64::from(y)
                    .saturating_mul(u64::from(frame.width))
                    .saturating_add(u64::from(x))
                    .saturating_mul(4),
            )?;
            let pixel = frame
                .bgra
                .get(offset..offset.saturating_add(4))
                .and_then(|bytes| bytes.try_into().ok())
                .context("Owned landmark lies outside the captured frame")?;
            ensure!(
                matches_pixel(pixel, expected),
                "A captured landmark shifted, stretched or changed geometry identity: {}x{} {:?}",
                frame.width,
                frame.height,
                frame.part
            );
        }
        Ok(())
    }

    impl Drop for Source {
        fn drop(&mut self) {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The owned fixture may already have exited; wait still reaps its process"
            )]
            let _ = self.0.kill();
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The fixture's lifetime is bounded by the parent even after acceptance fails"
            )]
            let _ = self.0.wait();
        }
    }
}
