use gpui::{Application, Platform};

#[cfg_attr(
    not(target_os = "windows"),
    expect(
        clippy::unnecessary_wraps,
        reason = "Only the Windows platform reports a startup failure"
    )
)]
#[expect(
    clippy::disallowed_types,
    reason = "GPUI's application owns its platform through Rc<dyn Platform>"
)]
pub(crate) fn new() -> anyhow::Result<Application> {
    let platform: std::rc::Rc<dyn Platform> = cfg_select! {
        target_os = "windows" => std::rc::Rc::new(anyhow::Context::context(
            gpui_windows::WindowsPlatform::new(false),
            "Cannot start the Windows interface",
        )?),
        target_os = "macos" => std::rc::Rc::new(gpui_macos::MacPlatform::new(false)),
        target_os = "linux" => gpui_linux::linux_platform(gpui::WindowingModes::X11),
    };
    Ok(Application::with_platform(platform))
}
