mod export;
mod handoff;
mod library;
mod menu;
mod narration;
mod navigation;
mod render;
mod source;
mod timeline;

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use export::Kind as ExportKind;
use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, ScrollHandle, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, px, size,
};
use library::{Detail, Key, Kind, Library, Mode};
use navigation::{Origin, Request};
use source::Root;
pub(crate) use source::Source;
use tokio::sync::watch;

use super::store::{recall, retention};
use crate::{
    gpui_ext::{EntityUpdate, Presence, defer_presence, follow},
    runtime::Snapshot,
    theme::Theme,
};

const JUMP: Duration = Duration::from_secs(10);

const DISCARD_AUDIO: &str = "This permanently removes the kept audio and its recovery checkpoint. \
                            Your saved Show stays.";
const DISCARD_SHOW: &str = "This permanently removes the interrupted Show's recovery data, \
                           including kept audio. Any saved copies stay.";

pub(crate) struct Actions {
    pub edit: fn(
        super::edits::Edit,
        Option<Arc<tempfile::TempDir>>,
        &mut App,
    ) -> anyhow::Result<super::edits::Receipt>,
    pub preview: fn(super::preview::Request, &mut App) -> anyhow::Result<super::preview::Work>,
    pub can_copy: fn(&App) -> bool,
    pub copy: fn(super::copy::Request, &mut App) -> anyhow::Result<super::copy::Receipt>,
    pub recover:
        fn(crate::show::Interrupted, &App) -> Result<crate::runtime::RecoveryReceipt, &'static str>,
}

enum ImageState {
    Empty,
    Loading,
    Ready(super::preview::Decoded),
    Failed(String),
}

pub(crate) struct Review {
    focus: FocusHandle,
    source: Source,
    theme: Theme,
    actions: Actions,
    library: Library,
    library_scroll: ScrollHandle,
    frame_scroll: ScrollHandle,
    directory: Option<PathBuf>,
    detail: Option<Detail>,
    mode: Mode,
    annotated: bool,
    timeline: timeline::Timeline,
    pane: Pane,
    request: Option<Request>,
    image: ImageState,
    notice: Option<String>,
    listing: Option<Task<()>>,
    opening: Option<Task<()>>,
    loading: Option<Task<()>>,
    copying: Option<Task<()>>,
    exporting: Option<Task<()>>,
    export_cancel: Option<async_channel::Sender<()>>,
    checking: Option<Task<()>>,
    keeping: Option<(PathBuf, Task<()>)>,
    recovering: Option<Task<()>>,
    discarding: Option<Task<()>>,
    confirm_discard: bool,
    keyboard: bool,
    menu: menu::Menu,
    visibility: Option<Task<()>>,
    _updates: Task<()>,
    _activation: Subscription,
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
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            focus,
            source,
            theme,
            actions,
            library: Library::default(),
            library_scroll: ScrollHandle::new(),
            frame_scroll: ScrollHandle::new(),
            directory: None,
            detail: None,
            mode: Mode::Keyframes,
            annotated: true,
            timeline: timeline::Timeline::default(),
            pane: Pane::Summary,
            request: None,
            image: ImageState::Empty,
            notice: None,
            listing: None,
            opening: None,
            loading: None,
            copying: None,
            exporting: None,
            export_cancel: None,
            checking: None,
            keeping: None,
            recovering: None,
            discarding: None,
            confirm_discard: false,
            keyboard: false,
            menu: menu::Menu::Closed,
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
        self.visibility = Some(defer_presence(window.window_handle(), Presence::Raised, cx));
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
        self.request = None;
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
                            Some(key) => view.open_selected(key, None, cx),
                            None => view.clear_selection(cx),
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
        cx: &mut Context<'_, Self>,
    ) {
        self.clear_selection(cx);
        if let Some(position) = self.library.position() {
            self.library_scroll.scroll_to_item(position);
        }
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
                    Some(at) => view.library.select(
                        key.directory,
                        detail.locate(at).map_or(0, |(index, _)| index),
                    ),
                    None => key,
                };
                view.detail = Some(detail);
                view.load_frame(key, cx);
                cx.notify();
            });
        }));
    }

    fn load_frame(&mut self, key: Key, cx: &mut Context<'_, Self>) {
        self.frame_scroll
            .scroll_to_item(key.frame.min(render::NEARBY_FRAMES));
        self.loading = None;
        if self.mode != Mode::Video || !matches!(self.image, ImageState::Ready(_)) {
            self.replace_image(ImageState::Loading, cx);
        }
        let Some(frame) = self
            .detail
            .as_ref()
            .and_then(|detail| detail.frames.get(key.frame))
        else {
            self.replace_image(ImageState::Empty, cx);
            return;
        };
        let request = super::preview::Request {
            directory: key.directory.clone(),
            content: frame.file.clone(),
            at: frame.at,
            annotated: self.annotated,
            fixture: self.fixture.clone(),
        };
        let work = match (self.actions.preview)(request, cx) {
            Ok(work) => work,
            Err(error) => {
                self.replace_image(
                    ImageState::Failed(format!("Cannot load this frame: {error:#}")),
                    cx,
                );
                cx.notify();
                return;
            },
        };
        self.loading = Some(cx.spawn(async move |view, cx| {
            let result = match work {
                super::preview::Work::Reading(receipt) => Some(receipt.await),
                super::preview::Work::Waiting(wait) => {
                    wait.await;
                    None
                },
            };
            view.update_if_alive(cx, |view, cx| {
                if !view.library.accepts(&key) {
                    return;
                }
                view.loading = None;
                let Some(result) = result else {
                    view.load_frame(key, cx);
                    return;
                };
                view.replace_image(
                    match result {
                        Ok(decoded) => ImageState::Ready(decoded),
                        Err(message) => ImageState::Failed(message),
                    },
                    cx,
                );
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn clear_selection(&mut self, cx: &mut Context<'_, Self>) {
        self.opening = None;
        self.loading = None;
        self.copying = None;
        self.checking = None;
        self.detail = None;
        self.request = None;
        self.confirm_discard = false;
        self.menu = menu::Menu::Closed;
        self.timeline = timeline::Timeline::default();
        self.notice = None;
        self.replace_image(ImageState::Empty, cx);
    }

    fn replace_image(&mut self, image: ImageState, cx: &mut App) {
        if let ImageState::Ready(replaced) = std::mem::replace(&mut self.image, image) {
            cx.defer(move |cx| cx.drop_image(replaced.image, None));
        }
    }

    fn read_failed(&mut self, error: io::Error, cx: &mut Context<'_, Self>) {
        if error.kind() == io::ErrorKind::NotFound {
            self.library.forget();
            self.clear_selection(cx);
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
        let Some(kept) = self.library.selected_row().and_then(|row| row.pinned) else {
            return;
        };

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
                    .and_then(|result| {
                        result.map_err(|error| {
                            error
                                .downcast::<io::Error>()
                                .unwrap_or_else(io::Error::other)
                        })
                    })
                    .and_then(|written| match written {
                        super::edits::Written::Retention(retention) => Ok(retention),
                        _ => Err(io::Error::other("Retention write returned no result")),
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

    pub(super) fn can_copy(&self, cx: &App) -> bool {
        self.source != Source::Demo && (self.actions.can_copy)(cx) && self.copying.is_none()
    }

    fn copy(&mut self, kind: CopyKind, cx: &mut Context<'_, Self>) {
        let Some(key) = self.library.selected.clone().filter(|_| self.can_copy(cx)) else {
            return;
        };
        let request = match kind {
            CopyKind::Agent => Ok(super::copy::Request::Agent(key.directory.clone())),
            CopyKind::Summary => Ok(super::copy::Request::Summary(key.directory.clone())),
            CopyKind::Evidence => match self.moment(&key) {
                Some(request) => request,
                None => return,
            },
        };
        let receipt = match request.and_then(|request| (self.actions.copy)(request, cx)) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.notice = Some(format!("Cannot copy this Show: {error:#}"));
                cx.notify();
                return;
            },
        };
        self.notice = Some(format!("Copying {}…", kind.name()));
        self.copying = Some(cx.spawn(async move |view, cx| {
            let result = receipt.await;
            let notice = match result {
                Ok(super::copy::Outcome::Copied) => kind.copied(),
                Ok(super::copy::Outcome::Missing) => {
                    "This Show is no longer available. Refresh the library.".into()
                },
                Err(error) => error,
            };
            view.update_if_alive(cx, |view, cx| {
                if view.library.is_selected(&key.directory) {
                    view.copying = None;
                    view.notice = Some(notice);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn moment(&self, key: &Key) -> Option<anyhow::Result<super::copy::Request>> {
        let detail = self.detail.as_ref()?;
        let frame = detail.frames.get(key.frame)?;
        let ImageState::Ready(decoded) = &self.image else {
            return None;
        };
        if self.loading.is_some() {
            return None;
        }
        let pointer = navigation::pointer(
            &key.directory,
            frame,
            self.mode,
            self.request,
            decoded.at,
            decoded.annotations,
        );
        Some(
            handoff::text(pointer, detail.summary.as_deref().ok(), &detail.narration).map(|text| {
                super::copy::Request::Moment {
                    text,
                    image: Arc::clone(&decoded.image),
                }
            }),
        )
    }

    fn update_summary(&mut self, summary: io::Result<String>) {
        if let Some(detail) = &mut self.detail {
            detail.summary = summary.map_err(|error| format!("Cannot read summary.md: {error}"));
        }
    }

    fn interrupted(&self) -> Option<&crate::show::Interrupted> {
        match &self.library.selected_row()?.kind {
            Kind::Interrupted(show) => Some(show),
            Kind::Saved => None,
        }
    }

    fn recover(&mut self, cx: &Context<'_, Self>) {
        if self.recovering.is_some() || self.discarding.is_some() || self.keeping.is_some() {
            return;
        }
        let Some(interrupted) = self.interrupted().cloned() else {
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
        let Some(interrupted) = self.interrupted().cloned() else {
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

    fn seek(&mut self, request: Request, cx: &mut Context<'_, Self>) {
        if let Some((index, _)) = self
            .detail
            .as_ref()
            .and_then(|detail| detail.locate(request.at))
        {
            self.act(Action::Frame(index), cx);
            self.request = Some(request);
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<'_, Self>) {
        self.menu = menu::Menu::Closed;
        match action {
            Action::Annotations => {
                if self.mode == Mode::Video
                    && let Some(current) = self.library.selected.clone()
                {
                    self.annotated = !self.annotated;
                    let key = self.library.select(current.directory, current.frame);
                    self.load_frame(key, cx);
                }
            },
            Action::Pane(pane) => self.pane = pane,
            Action::Clause(index) => {
                if let Some(clause) = self
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.narration.clauses().get(index))
                {
                    self.seek(
                        Request {
                            at: clause.at,
                            origin: Origin::Clause(index),
                        },
                        cx,
                    );
                }
            },
            Action::Mark(number) => {
                if matches!(self.mode, Mode::Source | Mode::Video)
                    && let Some((_, at)) = self
                        .detail
                        .as_ref()
                        .and_then(|detail| detail.marks.iter().find(|(mark, _)| *mark == number))
                {
                    self.seek(
                        Request {
                            at: *at,
                            origin: Origin::Mark(number),
                        },
                        cx,
                    );
                }
            },
            Action::Previous | Action::Next => {
                if let Some(key) = &self.library.selected {
                    let frame = if matches!(action, Action::Previous) {
                        key.frame.saturating_sub(1)
                    } else {
                        key.frame.saturating_add(1)
                    };
                    self.act(Action::Frame(frame), cx);
                }
            },
            Action::Refresh => self.refresh(cx),
            Action::Mode(mode) => {
                if self.mode != mode
                    && let Some(current) = self.library.selected.clone()
                {
                    let request = self.request;
                    let at = request.map(|request| request.at).or_else(|| {
                        self.detail
                            .as_ref()
                            .and_then(|detail| detail.frames.get(current.frame))
                            .map(|frame| frame.at)
                    });
                    self.mode = mode;
                    let key = self.library.select(current.directory, 0);
                    self.open_selected(key, at, cx);
                    self.request = request;
                }
            },
            Action::Show(index) => {
                if let Some(row) = self.library.rows.get(index)
                    && !self.library.is_selected(&row.directory)
                {
                    self.mode = Mode::Keyframes;
                    let key = self.library.select(row.directory.clone(), 0);
                    self.open_selected(key, None, cx);
                }
            },
            Action::Frame(index) => {
                self.request = None;
                if self
                    .detail
                    .as_ref()
                    .is_some_and(|detail| index < detail.frames.len())
                    && let Some(current) = self.library.selected.clone()
                    && current.frame != index
                {
                    let key = self.library.select(current.directory, index);
                    self.load_frame(key, cx);
                }
            },
            Action::Recover => self.recover(cx),
            Action::Discard => {
                self.confirm_discard = true;
                let message = if self
                    .interrupted()
                    .is_some_and(crate::show::Interrupted::narration_pending)
                {
                    DISCARD_AUDIO
                } else {
                    DISCARD_SHOW
                };
                self.notice = Some(message.into());
            },
            Action::ConfirmDiscard => self.discard(cx),
            Action::CancelDiscard => {
                self.confirm_discard = false;
                self.notice = None;
            },
            Action::Jump(direction) => {
                if let Some((detail, current)) =
                    self.detail.as_ref().zip(self.library.selected.as_ref())
                    && let Some(frame) = detail.frames.get(current.frame)
                {
                    let at = match direction {
                        Direction::Forward => frame.at.saturating_add(JUMP),
                        Direction::Back => frame.at.saturating_sub(JUMP),
                    };
                    let target = detail.locate(at).map_or(current.frame, |(index, _)| index);
                    let index = match direction {
                        Direction::Forward => target.max(current.frame.saturating_add(1)),
                        Direction::Back => target.min(current.frame.saturating_sub(1)),
                    }
                    .min(detail.frames.len().saturating_sub(1));
                    self.act(Action::Frame(index), cx);
                }
            },
            Action::Copy(kind) => self.copy(kind, cx),
            Action::Export(kind) => self.export(kind, cx),
            Action::CancelExport => {
                self.export_cancel = None;
                self.notice = Some("Cancelling video export…".into());
            },
            Action::Keep => self.keep(cx),
            Action::Folder => {
                if let Some(key) = &self.library.selected {
                    cx.open_with_system(&key.directory);
                }
            },
            Action::Movie => {
                if self
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.movie.is_some())
                    && let Some(key) = &self.library.selected
                {
                    cx.open_with_system(&key.directory.join(super::store::movie::FILE));
                }
            },
        }
        cx.notify();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Summary,
    Narration,
}

#[derive(Clone, Copy)]
enum CopyKind {
    Agent,
    Summary,
    Evidence,
}

impl CopyKind {
    fn name(self) -> &'static str {
        match self {
            Self::Agent => "a prompt for your agent",
            Self::Summary => "summary",
            Self::Evidence => "image, summary, and narration",
        }
    }

    fn copied(self) -> String {
        match self {
            Self::Agent => "Copied a prompt for your agent. Paste it into Claude Code or any \
                            coding agent; it points to this Show's files."
                .into(),
            Self::Summary | Self::Evidence => format!("Copied {}.", self.name()),
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Back,
    Forward,
}

#[derive(Clone, Copy)]
enum Action {
    Annotations,
    Pane(Pane),
    Clause(usize),
    Mark(usize),
    Recover,
    Discard,
    ConfirmDiscard,
    CancelDiscard,
    Jump(Direction),
    Mode(Mode),
    Previous,
    Next,
    Refresh,
    Show(usize),
    Frame(usize),
    Copy(CopyKind),
    Export(ExportKind),
    CancelExport,
    Keep,
    Folder,
    Movie,
}

#[cfg(test)]
mod tests;

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod native_tests;
