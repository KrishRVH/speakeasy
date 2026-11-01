//! Asymmetric owned landmarks expose crop offsets and stretching without saving screen pixels.

use anyhow::Context as _;
use objc2::{MainThreadMarker, MainThreadOnly as _, define_class, msg_send, rc::Retained};
use objc2_app_kit::{NSColor, NSRectFill, NSView, NSWindow};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

define_class!(
    // SAFETY: this main-thread view owns no borrowed state and contains panics at its native callback.
    #[unsafe(super = NSView)]
    #[thread_kind = objc2::MainThreadOnly]
    struct Pattern;

    // SAFETY: NSObjectProtocol adds no requirements.
    unsafe impl NSObjectProtocol for Pattern {}

    impl Pattern {
        #[unsafe(method(drawRect:))]
        fn draw(&self, _: NSRect) {
            #[expect(clippy::let_underscore_must_use, reason = "A failed synthetic draw is detected by missing landmarks; no panic can cross the native callback")]
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.paint()));
        }
    }
);

impl Pattern {
    fn paint(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let bounds = self.bounds();
        window.backgroundColor().setFill();
        NSRectFill(bounds);
        for ([x, y, width, height], [red, green, blue]) in [
            ([0.1, 0.2, 0.2, 0.1], [240.0, 240.0, 240.0]),
            ([0.7, 0.6, 0.1, 0.2], [220.0, 20.0, 220.0]),
        ] {
            NSColor::colorWithSRGBRed_green_blue_alpha(
                red / 255.0,
                green / 255.0,
                blue / 255.0,
                1.0,
            )
            .setFill();
            let rectangle = NSRect::new(
                NSPoint::new(
                    (x * bounds.size.width).round(),
                    (y * bounds.size.height).round(),
                ),
                NSSize::new(
                    (width * bounds.size.width).round(),
                    (height * bounds.size.height).round(),
                ),
            );
            NSRectFill(rectangle);
        }
    }
}

pub(super) fn attach(window: &NSWindow, main: MainThreadMarker) -> anyhow::Result<()> {
    let bounds = window
        .contentView()
        .context("Owned window has no content view")?
        .bounds();
    // SAFETY: this view is allocated and initialized on the main thread with owned fixture bounds.
    let view: Retained<Pattern> = unsafe { msg_send![Pattern::alloc(main), initWithFrame: bounds] };
    window.setContentView(Some(&view));
    Ok(())
}
