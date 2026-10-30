//! A native Show library with explicit retention controls. Selection owns its reads and one decoded
//! frame; private content stays out of the global asset cache, CLI output, and owner's last Show.

mod frame;
mod library;
mod render;
mod source;

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use anyhow::Context as _;
use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, RenderImage, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use library::{Detail, Key, Kind, Library, Mode};
use source::Root;
pub(crate) use source::Source;
use tokio::sync::watch;

use super::store::{recall, retention};
use crate::{
    gpui_ext::{AppUpdate, EntityUpdate, defer_native, follow},
    runtime::Snapshot,
    theme::Theme,
};

pub(crate) struct Actions {
    pub edit: fn(
        super::edits::Edit,
        Option<Arc<tempfile::TempDir>>,
        &mut App,
    ) -> anyhow::Result<super::edits::Receipt>,
    pub can_copy: fn(&App) -> bool,
    pub recover:
        fn(crate::show::Interrupted, &App) -> Result<crate::runtime::RecoveryReceipt, &'static str>,
}

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
    actions: Actions,
    library: Library,
    directory: Option<PathBuf>,
    detail: Option<Detail>,
    mode: Mode,
    image: ImageState,
    notice: Option<String>,
    listing: Option<Task<()>>,
    opening: Option<Task<()>>,
    loading: Option<Task<()>>,
    copying: Option<Task<()>>,
    checking: Option<Task<()>>,
    keeping: Option<(PathBuf, Task<()>)>,
    recovering: Option<Task<()>>,
    discarding: Option<Task<()>>,
    confirm_discard: bool,
    visibility: Option<Task<()>>,
    _updates: Task<()>,
    _activation: Subscription,
    // Immutable leases keep the synthetic folder alive during accepted writes.
    fixture: Option<Arc<tempfile::TempDir>>,
}

pub(crate) fn open(
    source: Source,
    theme: Theme,
    snapshots: watch::Receiver<Snapshot>,
    actions: Actions,
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
        |window, cx| cx.new(|cx| Review::new(source, theme, snapshots, actions, window, cx)),
    )?;
    handle.update(cx, |view, window, cx| view.reveal(window, cx))?;
    Ok(handle)
}

impl Review {
    fn new(
        source: Source,
        theme: Theme,
        snapshots: watch::Receiver<Snapshot>,
        actions: Actions,
        window: &mut Window,
        cx: &Context<'_, Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            focus,
            source,
            theme,
            actions,
            library: Library::default(),
            directory: None,
            detail: None,
            mode: Mode::Keyframes,
            image: ImageState::Empty,
            notice: None,
            listing: None,
            opening: None,
            loading: None,
            copying: None,
            checking: None,
            keeping: None,
            recovering: None,
            discarding: None,
            confirm_discard: false,
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
        if self.keeping.is_some() {
            return;
        }
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
                    Ok((root, (rows, notice))) => {
                        view.directory = Some(root.directory);
                        view.fixture = root.fixture.or_else(|| view.fixture.take());
                        match view.library.replace(rows) {
                            Some(key) => view.open_selected(key, None, None, cx),
                            None => view.clear_selection(None, cx),
                        }
                        view.notice = notice;
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

    fn open_selected(
        &mut self,
        key: Key,
        seek: Option<speakeasy_show::Moment>,
        window: Option<&mut Window>,
        cx: &mut Context<'_, Self>,
    ) {
        self.clear_selection(window, cx);
        if self.interrupted().is_some() {
            cx.notify();
            return;
        }
        let directory = key.directory.clone();
        let mode = self.mode;
        let work = cx
            .background_executor()
            .spawn(async move { Detail::open(&directory, mode) });
        self.opening = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            view.update_if_alive(cx, |view, cx| {
                if !view.library.accepts(&key) {
                    return;
                }
                view.opening = None;
                let detail = match result {
                    Ok(detail) => detail,
                    Err(error) => {
                        view.read_failed(error, cx);
                        cx.notify();
                        return;
                    },
                };
                let key = match seek {
                    Some(at) => view.library.select(key.directory, detail.frame_at(at)),
                    None => key,
                };
                view.detail = Some(detail);
                view.load_frame(key, None, cx);
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
        self.confirm_discard = false;
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
        if self.keeping.is_some() {
            return;
        }
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
            Ok((
                recall::summary(&directory),
                super::store::pinned(&directory).ok(),
            ))
        });
        self.checking = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            view.update_if_alive(cx, |view, cx| {
                if !view.library.is_selected(&key.directory) {
                    return;
                }
                view.checking = None;
                match result {
                    Ok((summary, pinned)) => {
                        view.update_summary(summary);
                        view.library.update_retention(&key.directory, pinned);
                    },
                    Err(error) => view.read_failed(error, cx),
                }
                cx.notify();
            });
        }));
    }

    fn keep(&mut self, cx: &mut Context<'_, Self>) {
        if self.keeping.is_some()
            || self.listing.is_some()
            || self.recovering.is_some()
            || self.discarding.is_some()
        {
            return;
        }
        let Some((parent, key)) = self.directory.clone().zip(self.library.selected.clone()) else {
            return;
        };
        let Some(kept) = self
            .library
            .rows
            .iter()
            .find(|row| row.directory == key.directory)
            .and_then(|row| row.pinned)
        else {
            return;
        };
        // A read begun before this write must not restore the previous retention afterward.
        self.checking = None;
        let directory = key.directory.clone();
        let work = match (self.actions.edit)(
            super::edits::Edit::Keep {
                parent,
                directory,
                kept: !kept,
            },
            self.fixture.clone(),
            cx,
        ) {
            Ok(work) => work,
            Err(error) => {
                self.notice = Some(format!("Cannot change retention: {error:#}"));
                return;
            },
        };
        self.keeping = Some((
            key.directory.clone(),
            cx.spawn(async move |view, cx| {
                let result = work
                    .recv()
                    .await
                    .map_err(io::Error::other)
                    .and_then(|result| result.map_err(io::Error::other))
                    .and_then(|retention| {
                        retention
                            .ok_or_else(|| io::Error::other("Retention write returned no result"))
                    });
                view.update_if_alive(cx, |view, cx| {
                    view.keeping = None;
                    view.kept(&key.directory, result, cx);
                    cx.notify();
                });
            }),
        ));
        cx.notify();
    }

    fn kept(
        &mut self,
        directory: &Path,
        result: io::Result<retention::Retention>,
        cx: &mut Context<'_, Self>,
    ) {
        if let Ok(retention) = &result {
            self.library
                .update_retention(directory, Some(*retention == retention::Retention::Kept));
        }
        // A completed action's notice belongs only to the Show it targeted.
        if !self.library.is_selected(directory) {
            return;
        }
        match result {
            Ok(retention) => {
                self.notice = Some(format!("{}.", retention.label(SystemTime::now())));
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.read_failed(error, cx),
            Err(error) => {
                self.notice = Some(format!(
                    "Cannot change this Show's retention: {error}. {}",
                    crate::paths::remedy(&error)
                ));
            },
        }
    }

    fn copy(&mut self, cx: &mut Context<'_, Self>) {
        if self.source == Source::Demo || !(self.actions.can_copy)(cx) || self.copying.is_some() {
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
                    view.library.is_selected(&key.directory) && (view.actions.can_copy)(cx)
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

    fn interrupted(&self) -> Option<crate::show::Interrupted> {
        let key = self.library.selected.as_ref()?;
        match &self
            .library
            .rows
            .iter()
            .find(|row| row.directory == key.directory)?
            .kind
        {
            Kind::Interrupted(show) => Some(show.clone()),
            Kind::Saved => None,
        }
    }

    fn recover(&mut self, cx: &Context<'_, Self>) {
        if self.recovering.is_some() || self.discarding.is_some() || self.keeping.is_some() {
            return;
        }
        let Some(interrupted) = self.interrupted() else {
            return;
        };
        let key = self.library.selected.clone();
        let result = match (self.actions.recover)(interrupted, cx) {
            Ok(result) => result,
            Err(error) => {
                self.notice = Some(error.into());
                return;
            },
        };
        self.confirm_discard = false;
        self.notice =
            Some("Recovery queued. It starts after the current recording finishes.".into());
        self.recovering = Some(cx.spawn(async move |view, cx| {
            let result = result.recv().await.unwrap_or_else(|_closed| {
                Err("Recovery stopped. The interrupted Show remains available.".into())
            });
            view.update_if_alive(cx, |view, cx| {
                view.recovering = None;
                match result {
                    Ok(show) if view.library.selected == key => {
                        view.library.select(show.directory().to_owned(), 0);
                        view.mode = Mode::Keyframes;
                        view.refresh(cx);
                    },
                    Ok(_) => view.refresh(cx),
                    Err(error) if view.library.selected == key => {
                        view.notice = Some(error);
                        cx.notify();
                    },
                    Err(_) => cx.notify(),
                }
            });
        }));
    }

    fn discard(&mut self, cx: &mut Context<'_, Self>) {
        if self.recovering.is_some() || self.discarding.is_some() || self.keeping.is_some() {
            return;
        }
        let Some(interrupted) = self.interrupted() else {
            return;
        };
        if !self.confirm_discard {
            return;
        }
        let key = self.library.selected.clone();
        let work = match (self.actions.edit)(
            super::edits::Edit::Discard(interrupted),
            self.fixture.clone(),
            cx,
        ) {
            Ok(work) => work,
            Err(error) => {
                self.notice = Some(format!("Cannot discard this Show: {error:#}"));
                return;
            },
        };
        self.discarding = Some(cx.spawn(async move |view, cx| {
            let result = work
                .recv()
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            view.update_if_alive(cx, |view, cx| {
                view.discarding = None;
                view.confirm_discard = false;
                match result {
                    Ok(_) => view.refresh(cx),
                    Err(error) if view.library.selected == key => {
                        view.notice = Some(format!("Cannot discard this Show: {error:#}"));
                        cx.notify();
                    },
                    Err(_) => cx.notify(),
                }
            });
        }));
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
            Action::Mode(mode) => {
                if self.mode != mode
                    && let Some(current) = self.library.selected.clone()
                {
                    let at = self
                        .detail
                        .as_ref()
                        .and_then(|detail| detail.frames.get(current.frame))
                        .map(|frame| frame.at);
                    self.mode = mode;
                    let key = self.library.select(current.directory, 0);
                    self.open_selected(key, at, Some(window), cx);
                }
            },
            Action::Show(index) => {
                if let Some(row) = self.library.rows.get(index)
                    && !self.library.is_selected(&row.directory)
                {
                    self.mode = Mode::Keyframes;
                    let key = self.library.select(row.directory.clone(), 0);
                    self.open_selected(key, None, Some(window), cx);
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
            Action::Recover => self.recover(cx),
            Action::Discard => {
                self.confirm_discard = true;
                self.notice = Some("Discard permanently removes this interrupted Show.".into());
            },
            Action::ConfirmDiscard => self.discard(cx),
            Action::CancelDiscard => {
                self.confirm_discard = false;
                self.notice = None;
            },
            Action::Jump(forward) => {
                if let Some((detail, current)) =
                    self.detail.as_ref().zip(self.library.selected.as_ref())
                    && let Some(frame) = detail.frames.get(current.frame)
                {
                    let ms = if forward {
                        frame.at.as_millis().saturating_add(10_000)
                    } else {
                        frame.at.as_millis().saturating_sub(10_000)
                    };
                    let target = detail.frame_at(speakeasy_show::Moment::from_millis(ms));
                    let index = if forward {
                        target.max(current.frame.saturating_add(1))
                    } else {
                        target.min(current.frame.saturating_sub(1))
                    }
                    .min(detail.frames.len().saturating_sub(1));
                    self.act(Action::Frame(index), window, cx);
                }
            },
            Action::Copy => self.copy(cx),
            Action::Keep => self.keep(cx),
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
    Recover,
    Discard,
    ConfirmDiscard,
    CancelDiscard,
    Jump(bool),
    Mode(Mode),
    Previous,
    Next,
    Refresh,
    Show(usize),
    Frame(usize),
    Copy,
    Keep,
    Folder,
}

#[cfg(test)]
mod tests;

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod native_tests;
