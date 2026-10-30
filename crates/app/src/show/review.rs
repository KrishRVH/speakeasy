//! A native, read-only Show library. Selection owns its reads and one decoded frame; no private
//! content enters the global asset cache, the CLI projections, or the session owner's last Show.

mod frame;
mod library;
mod render;
mod source;

use std::{io, path::PathBuf, sync::Arc};

use anyhow::Context as _;
use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, RenderImage, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use library::{Detail, Key, Library};
use source::Root;
pub(crate) use source::Source;
use tokio::sync::watch;

use super::store::recall;
use crate::{
    gpui_ext::{AppUpdate, EntityUpdate, defer_native, follow},
    runtime::Snapshot,
    theme::Theme,
};

enum ImageState {
    Empty,
    Loading,
    Ready(Arc<RenderImage>),
    Failed(String),
}

/// Inputs come from the shell; the review view never reaches into its global Services owner.
pub(crate) struct Review {
    focus: FocusHandle,
    source: Source,
    theme: Theme,
    can_copy: fn(&App) -> bool,
    library: Library,
    directory: Option<PathBuf>,
    detail: Option<Detail>,
    image: ImageState,
    notice: Option<String>,
    listing: Option<Task<()>>,
    opening: Option<Task<()>>,
    loading: Option<Task<()>>,
    copying: Option<Task<()>>,
    checking: Option<Task<()>>,
    visibility: Option<Task<()>>,
    _updates: Task<()>,
    _activation: Subscription,
    // Pending tasks drop before the synthetic folder; already-running reads may finish later.
    fixture: Option<tempfile::TempDir>,
}

pub(crate) fn open(
    source: Source,
    theme: Theme,
    snapshots: watch::Receiver<Snapshot>,
    can_copy: fn(&App) -> bool,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Review>> {
    let available = cx.primary_display().map(|display| display.bounds().size);
    let width = available.map_or(px(1_100.0), |size| {
        (size.width - px(80.0)).clamp(px(640.0), px(1_100.0))
    });
    let height = available.map_or(px(800.0), |size| {
        (size.height - px(100.0)).clamp(px(460.0), px(800.0))
    });
    let handle = cx.open_window(
        WindowOptions {
            #[cfg(target_os = "linux")]
            app_id: Some(speakeasy_platform::APPLICATION_ID.into()),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(width, height),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Speakeasy Shows".into()),
                ..Default::default()
            }),
            show: false,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Review::new(source, theme, snapshots, can_copy, window, cx)),
    )?;
    handle.update(cx, |view, window, cx| view.reveal(window, cx))?;
    Ok(handle)
}

impl Review {
    fn new(
        source: Source,
        theme: Theme,
        snapshots: watch::Receiver<Snapshot>,
        can_copy: fn(&App) -> bool,
        window: &mut Window,
        cx: &Context<'_, Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            focus,
            source,
            theme,
            can_copy,
            library: Library::default(),
            directory: None,
            detail: None,
            image: ImageState::Empty,
            notice: None,
            listing: None,
            opening: None,
            loading: None,
            copying: None,
            checking: None,
            visibility: None,
            _updates: follow(
                snapshots,
                cx,
                |snapshot| snapshot.phase,
                |_: &mut Self, _, cx| cx.notify(),
            ),
            _activation: cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.check_selected(cx);
                }
            }),
            fixture: None,
        }
    }

    pub(crate) fn reveal(&mut self, window: &Window, cx: &mut Context<'_, Self>) {
        self.refresh(cx);
        self.visibility = Some(defer_native(
            window.window_handle(),
            "Shows",
            cx,
            |native, cx| {
                native.set_window_visible(true);
                cx.update_if_running(|cx| cx.activate(true));
            },
        ));
    }

    pub(crate) fn set_theme(&mut self, theme: Theme, cx: &mut Context<'_, Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        self.notice = None;
        let source = self.source;
        let existing = self.directory.clone();
        let work = cx.background_executor().spawn(async move {
            let root = match existing {
                Some(directory) => Root {
                    directory,
                    fixture: None,
                },
                None => source.load()?,
            };
            let rows = library::list(&root.directory)
                .with_context(|| format!("Cannot open {}", root.directory.display()))?;
            anyhow::Ok((root, rows))
        });
        self.listing = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            view.update_if_alive(cx, |view, cx| {
                view.listing = None;
                match result {
                    Ok((root, rows)) => {
                        view.directory = Some(root.directory);
                        view.fixture = root.fixture.or_else(|| view.fixture.take());
                        match view.library.replace(rows) {
                            Some(key) => view.open_selected(key, None, cx),
                            None => view.clear_selection(None, cx),
                        }
                    },
                    Err(error) => {
                        const REMEDY: &str = "Check that the folder is readable, then refresh.";
                        view.notice = Some(format!("Cannot read saved Shows: {error:#}. {REMEDY}"));
                    },
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn open_selected(&mut self, key: Key, window: Option<&mut Window>, cx: &mut Context<'_, Self>) {
        self.clear_selection(window, cx);
        let directory = key.directory.clone();
        let work = cx
            .background_executor()
            .spawn(async move { Detail::open(&directory) });
        self.opening = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            view.update_if_alive(cx, |view, cx| {
                if !view.library.accepts(&key) {
                    return;
                }
                view.opening = None;
                match result {
                    Ok(detail) => {
                        view.detail = Some(detail);
                        view.load_frame(key, None, cx);
                    },
                    Err(error) => view.read_failed(error, cx),
                }
                cx.notify();
            });
        }));
    }

    fn load_frame(&mut self, key: Key, window: Option<&mut Window>, cx: &mut Context<'_, Self>) {
        self.loading = None;
        self.replace_image(ImageState::Loading, window, cx);
        let Some(frame) = self
            .detail
            .as_ref()
            .and_then(|detail| detail.frames.get(key.frame))
        else {
            self.replace_image(ImageState::Empty, None, cx);
            return;
        };
        let (directory, name, renderer) =
            (key.directory.clone(), frame.file.clone(), cx.svg_renderer());
        let work = cx.background_executor().spawn(async move {
            frame::read(&directory, &name)?
                .to_image_data(renderer)
                .map_err(|_private_error| {
                    "This frame could not be decoded as a PNG image.".to_owned()
                })
        });
        self.loading = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            view.update_if_alive(cx, |view, cx| {
                if !view.library.accepts(&key) {
                    return;
                }
                view.loading = None;
                view.replace_image(
                    match result {
                        Ok(image) => ImageState::Ready(image),
                        Err(message) => ImageState::Failed(message),
                    },
                    None,
                    cx,
                );
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn clear_selection(&mut self, window: Option<&mut Window>, cx: &mut Context<'_, Self>) {
        self.opening = None;
        self.loading = None;
        self.copying = None;
        self.checking = None;
        self.detail = None;
        self.notice = None;
        self.replace_image(ImageState::Empty, window, cx);
    }

    fn replace_image(&mut self, image: ImageState, window: Option<&mut Window>, cx: &mut App) {
        if let ImageState::Ready(old) = std::mem::replace(&mut self.image, image) {
            cx.drop_image(old, window);
        }
    }

    fn read_failed(&mut self, error: io::Error, cx: &mut Context<'_, Self>) {
        if error.kind() == io::ErrorKind::NotFound {
            self.library.forget();
            self.clear_selection(None, cx);
        } else {
            self.notice = Some(format!(
                "Cannot open this Show: {error}. {}",
                crate::paths::remedy(&error)
            ));
        }
    }

    fn check_selected(&mut self, cx: &Context<'_, Self>) {
        let Some(key) = self
            .library
            .selected
            .clone()
            .filter(|_| self.detail.is_some())
        else {
            return;
        };
        let directory = key.directory.clone();
        let work = cx.background_executor().spawn(async move {
            std::fs::metadata(&directory)?;
            Ok(recall::summary(&directory))
        });
        self.checking = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            view.update_if_alive(cx, |view, cx| {
                if !view.library.is_selected(&key.directory) {
                    return;
                }
                view.checking = None;
                match result {
                    Ok(summary) => view.update_summary(summary),
                    Err(error) => view.read_failed(error, cx),
                }
                cx.notify();
            });
        }));
    }

    fn copy(&mut self, cx: &mut Context<'_, Self>) {
        if self.source == Source::Demo || !(self.can_copy)(cx) || self.copying.is_some() {
            return;
        }
        let Some(key) = self.library.selected.clone() else {
            return;
        };
        let directory = key.directory.clone();
        let read = cx
            .background_executor()
            .spawn(async move { recall::summary(&directory) });
        self.notice = Some("Copying summary…".into());
        self.copying = Some(cx.spawn(async move |view, cx| {
            let summary = read.await;
            // Changing Shows cancels this task. Frame navigation within the same Show does not
            // invalidate an explicit copy or leave its progress notice stuck.
            let allowed = view
                .update(cx, |view, cx| {
                    view.library.is_selected(&key.directory) && (view.can_copy)(cx)
                })
                .unwrap_or(false);
            let result = if allowed {
                cx.background_executor()
                    .spawn(async move { copy_summary(summary) })
                    .await
            } else {
                Err("Finish the current recording before copying a Show.".into())
            };
            view.update_if_alive(cx, |view, cx| {
                if view.library.is_selected(&key.directory) {
                    view.copying = None;
                    view.notice =
                        Some(result.map_or_else(|error| error, |()| "Summary copied.".into()));
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn update_summary(&mut self, summary: io::Result<String>) {
        if let Some(detail) = &mut self.detail {
            detail.summary = summary.map_err(|error| format!("Cannot read summary.md: {error}"));
        }
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<'_, Self>) {
        match action {
            Action::Previous | Action::Next => {
                if let Some(key) = &self.library.selected {
                    let frame = if matches!(action, Action::Previous) {
                        key.frame.saturating_sub(1)
                    } else {
                        key.frame.saturating_add(1)
                    };
                    self.act(Action::Frame(frame), window, cx);
                }
            },
            Action::Refresh => self.refresh(cx),
            Action::Show(index) => {
                if let Some(row) = self.library.rows.get(index)
                    && !self.library.is_selected(&row.directory)
                {
                    let key = self.library.select(row.directory.clone(), 0);
                    self.open_selected(key, Some(window), cx);
                }
            },
            Action::Frame(index) => {
                if self
                    .detail
                    .as_ref()
                    .is_some_and(|detail| index < detail.frames.len())
                    && let Some(current) = self.library.selected.clone()
                    && current.frame != index
                {
                    let key = self.library.select(current.directory, index);
                    self.load_frame(key, Some(window), cx);
                }
            },
            Action::Copy => self.copy(cx),
            Action::Folder => {
                if let Some(key) = &self.library.selected {
                    cx.open_with_system(&key.directory);
                }
            },
        }
        cx.notify();
    }
}

fn copy_summary(summary: io::Result<String>) -> Result<(), String> {
    const REMEDY: &str = "Close any app holding the clipboard, then try again.";
    let text = summary.map_err(|error| format!("Cannot read summary.md: {error}"))?;
    speakeasy_platform::copy_text(&text)
        .map_err(|error| format!("Cannot copy the summary: {error:#}. {REMEDY}"))
}

#[derive(Clone, Copy)]
enum Action {
    Previous,
    Next,
    Refresh,
    Show(usize),
    Frame(usize),
    Copy,
    Folder,
}

#[cfg(test)]
mod tests;

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod native_tests;
