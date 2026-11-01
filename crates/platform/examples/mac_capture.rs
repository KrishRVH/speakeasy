//! Explicit owned-window screen acceptance. It neither saves pixels nor accesses audio or input hooks.

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
        NSWindowStyleMask,
    };
    use objc2_core_graphics::{CGDisplayBounds, CGError, CGGetActiveDisplayList, CGMainDisplayID};
    use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};
    use speakeasy_platform::{Display, Part, ScreenCapture, ScreenEvent, Target};

    pub(super) fn run() -> anyhow::Result<()> {
        let mode = std::env::args()
            .nth(1)
            .context("Use --owned-screen to opt into synthetic window capture")?;
        ensure!(
            mode == "--owned-screen" || mode == "--source",
            "Use --owned-screen to opt into synthetic window capture"
        );
        let main = MainThreadMarker::new()
            .context("Native window acceptance must start on the main thread")?;
        let app = NSApplication::sharedApplication(main);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        if mode == "--source" {
            return source(main);
        }
        let mut source = Source(
            Command::new(std::env::current_exe()?)
                .arg("--source")
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
        ensure!(
            ready.trim() == "ready",
            "Synthetic window did not become ready"
        );
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
                    .and_then(|runtime| runtime.block_on(verify(display, part)));
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

    fn source(main: MainThreadMarker) -> anyhow::Result<()> {
        let window = window(main, [100.0, 100.0, 400.0, 300.0], 100)?;
        window.setBackgroundColor(Some(&color([230, 30, 20])));
        window.orderFrontRegardless();
        window.display();
        writeln!(std::io::stdout().lock(), "ready")?;
        std::io::stdout().lock().flush()?;
        let started = Instant::now();
        let mut previous = 0;
        loop {
            let phase = (started.elapsed().as_millis() / 200) % 2;
            if phase != previous {
                window.setBackgroundColor(Some(&color(if phase == 0 {
                    [230, 30, 20]
                } else {
                    [20, 220, 30]
                })));
                window.display();
                previous = phase;
            }
            pump();
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

    async fn verify(display: Display, part: Part) -> anyhow::Result<()> {
        for _ in 0..2 {
            let (events, frames) = async_channel::bounded(3);
            let capture = ScreenCapture::start(
                Target::area(display, part),
                Duration::from_millis(33),
                2560,
                events,
                None,
            )?;
            let result = inspect(&frames, part).await;
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
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        let mut seen = [false; 2];
        let mut previous = None;
        let mut count = 0_u32;
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
                frame.part == part && frame.width > 0 && frame.height > 0,
                "Owned capture changed its fixed region"
            );
            ensure!(
                frame.at <= Instant::now() && previous.is_none_or(|last| frame.at > last),
                "Native timestamps are not monotonic presentation times"
            );
            previous = Some(frame.at);
            let patterns = [[20, 30, 230], [30, 220, 20]];
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
            "The owned capture missed changing content"
        );
        Ok(())
    }

    struct Source(Child);

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
