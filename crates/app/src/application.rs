use gpui::{Application, Global, Platform};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Presentation {
    #[cfg(target_os = "linux")]
    pub(crate) display_server: speakeasy_platform::DisplayServer,
}

impl Global for Presentation {}

#[cfg(target_os = "linux")]
impl Presentation {
    fn windowing(self) -> gpui::WindowingModes {
        match self.display_server {
            speakeasy_platform::DisplayServer::Wayland => gpui::WindowingModes::WAYLAND,
            speakeasy_platform::DisplayServer::X11 => gpui::WindowingModes::X11,
        }
    }
}

#[cfg_attr(
    target_os = "macos",
    expect(
        clippy::unnecessary_wraps,
        reason = "Only the Windows and Linux platforms report a startup failure"
    )
)]
#[expect(
    clippy::disallowed_types,
    reason = "GPUI's application owns its platform through Rc<dyn Platform>"
)]
pub(crate) fn new() -> anyhow::Result<(Application, Presentation)> {
    let presentation = Presentation {
        #[cfg(target_os = "linux")]
        display_server: speakeasy_platform::display_server()?,
    };
    let platform: std::rc::Rc<dyn Platform> = cfg_select! {
        target_os = "windows" => std::rc::Rc::new(anyhow::Context::context(
            gpui_windows::WindowsPlatform::new(false),
            "Cannot start the Windows interface",
        )?),
        target_os = "macos" => std::rc::Rc::new(gpui_macos::MacPlatform::new(false)),
        target_os = "linux" => gpui_linux::linux_platform(presentation.windowing()),
    };
    let application = Application::with_platform(platform);
    Ok((application, presentation))
}
