use gpui::{AnyElement, Div, SharedString, div, prelude::*, px, rgb};
use speakeasy_platform::Permission;

use super::Ui;
use crate::{
    controls::{Weight, icon},
    icons,
    shell::{
        Services,
        settings::{
            Action, Reset, Setting, Settings, is_demo, onboarding, permissions, updates::Update,
        },
    },
    theme::Theme,
};

impl Settings {
    pub(super) fn general_page(&self, ui: &Ui<'_>) -> Vec<AnyElement> {
        let startup = cfg!(target_os = "windows") && ui.cx.global::<Services>().default_settings;
        #[cfg(target_os = "linux")]
        let desktop = Some(self.desktop_section(ui).into_any_element());
        #[cfg(not(target_os = "linux"))]
        let desktop = None;
        [
            Some(self.look_section(ui).into_any_element()),
            startup.then(|| self.startup_section(ui).into_any_element()),
            desktop,
            Some(self.privacy_section(ui).into_any_element()),
            Some(self.about_section(ui).into_any_element()),
            (!self.unreadable).then(|| self.start_over(ui).into_any_element()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    fn look_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let theme = self.config.theme;
        let swatches = div()
            .id("themes")
            .role(gpui::Role::RadioGroup)
            .aria_label("Theme")
            .flex()
            .gap(px(4.0))
            .children(Theme::ALL.map(|choice| {
                let action = Action::Theme(choice);
                kit.swatch(
                    action.element_id(),
                    choice.name(),
                    choice.palette(),
                    choice == theme,
                    Some(ui.press(action)),
                )
            }));
        let switch = |title: &'static str, detail: &'static str, setting: Setting| {
            kit.setting(
                title,
                Some(detail.into()),
                ui.switch(title, setting.is_on(&self.config), Action::Toggle(setting)),
            )
            .into_any_element()
        };
        kit.section(
            "Look and feel",
            vec![
                kit.setting("Theme", Some(theme.name().into()), swatches)
                    .into_any_element(),
                switch(
                    "Reduce motion",
                    "Calmer pill and window animations.",
                    Setting::ReduceMotion,
                ),
                switch(
                    "Play sounds",
                    "Soft tones when dictation starts, stops, or cancels.",
                    Setting::PlaySounds,
                ),
            ],
        )
    }

    fn startup_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let blocked = self.config.start_at_login && self.startup_apps_off;
        kit.section(
            "Startup",
            vec![
                kit.setting(
                    "Start at login",
                    Some(
                        if blocked {
                            "Turned off in Windows' Startup apps. Turn it on there."
                        } else {
                            "Speakeasy waits in the tray after you sign in."
                        }
                        .into(),
                    ),
                    ui.switch(
                        "Start at login",
                        self.config.start_at_login,
                        Action::Toggle(Setting::StartAtLogin),
                    ),
                )
                .into_any_element(),
            ],
        )
    }

    #[cfg(target_os = "linux")]
    fn desktop_section(&self, ui: &Ui<'_>) -> Div {
        use crate::shell::settings::menu::{self, Menu};
        let command = &self.desktop_command;
        let external = self.config.linux.external_shortcut;
        let detail: SharedString = if external {
            format!(
                "Assign {command} --toggle and {command} --cancel in your desktop's keyboard settings."
            )
            .into()
        } else {
            format!(
                "Speakeasy asks your desktop for {}.",
                self.presentation.shortcut
            )
            .into()
        };
        let kit = ui.kit();
        kit.section(
            "Desktop",
            vec![
                kit.setting(
                    "Shortcuts",
                    Some(detail),
                    self.dropdown(
                        ui,
                        Menu::Bindings,
                        "Shortcuts",
                        menu::bindings_label(external),
                    ),
                )
                .into_any_element(),
            ],
        )
    }

    fn privacy_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let rows = self.permissions.map(|granted| {
            let row = |name: &'static str, state: Permission, help: permissions::Help| {
                permission_row(ui, name, state, help).into_any_element()
            };
            let rows = vec![row(
                "Microphone",
                granted.microphone,
                permissions::Help::Microphone,
            )];
            #[cfg(target_os = "macos")]
            let rows = {
                let mut rows = rows;
                rows.push(row(
                    "Accessibility",
                    granted.accessibility,
                    permissions::Help::Accessibility,
                ));
                rows
            };
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            let rows = {
                let mut rows = rows;
                rows.push(row(
                    "Screen, for Shows",
                    granted.screen,
                    permissions::Help::Screen,
                ));
                rows
            };
            rows
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(kit.heading("Privacy"))
                    .child(ui.text(
                        "Speech becomes text on this computer. No account, no dictation history, no uploads.",
                    )),
            )
            .when_some(rows, |section, rows| section.child(kit.group(rows)))
    }

    fn about_section(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let detail: SharedString = match &self.update {
            Update::Idle => "Checks GitHub only when you ask. Installing stays your choice.".into(),
            Update::Checking { .. } => "Checking the latest GitHub release…".into(),
            Update::Available(version) => format!("Speakeasy {version} is available.").into(),
            #[cfg(target_os = "windows")]
            Update::Downloading {
                cancelled: false, ..
            } => "Downloading and verifying the update…".into(),
            #[cfg(target_os = "windows")]
            Update::Downloading { .. } => "Cancelling the download…".into(),
            #[cfg(target_os = "windows")]
            Update::Ready(_) => "Update verified. Installing restarts Speakeasy.".into(),
        };
        let controls = div().flex().flex_none().items_center().gap(px(8.0));
        let controls = match &self.update {
            Update::Idle | Update::Checking { .. } => controls.child(ui.button_if(
                if self.update.busy() {
                    "Checking…"
                } else {
                    "Check for updates"
                },
                Action::CheckUpdates,
                Weight::Secondary,
                !self.update.busy() && !is_demo(ui.cx),
            )),
            Update::Available(_) => {
                #[cfg(target_os = "windows")]
                let download = (crate::updates::Download::available() && !self.resetting())
                    .then(|| ui.button("Download", Action::DownloadUpdate, Weight::Primary));
                #[cfg(not(target_os = "windows"))]
                let download: Option<gpui::Stateful<Div>> = None;
                let notes = if download.is_some() {
                    Weight::Quiet
                } else {
                    Weight::Primary
                };
                controls
                    .child(ui.button("Release notes", Action::OpenUpdate, notes))
                    .children(download)
            },
            #[cfg(target_os = "windows")]
            Update::Downloading { cancelled, .. } => controls.child(ui.button_if(
                "Cancel",
                Action::CancelUpdate,
                Weight::Quiet,
                !cancelled,
            )),
            #[cfg(target_os = "windows")]
            Update::Ready(_) => controls
                .child(ui.button("Cancel", Action::CancelUpdate, Weight::Quiet))
                .child(ui.button(
                    "Install and restart",
                    Action::InstallUpdate,
                    Weight::Primary,
                )),
        };
        kit.section(
            "About",
            vec![
                kit.setting(
                    format!("Speakeasy {}", env!("CARGO_PKG_VERSION")),
                    Some(detail),
                    controls,
                )
                .into_any_element(),
                kit.setting(
                    "Setup guide",
                    Some("Walk through the shortcut and a practice sentence again.".into()),
                    ui.button_if(
                        "Replay",
                        Action::Onboarding(onboarding::Action::Replay),
                        Weight::Secondary,
                        !is_demo(ui.cx),
                    ),
                )
                .into_any_element(),
            ],
        )
    }

    fn start_over(&self, ui: &Ui<'_>) -> Div {
        let kit = ui.kit();
        let enabled = self.can_reset(ui.cx);
        let control = match self.reset {
            Reset::Idle => ui
                .button_if("Start over…", Action::Reset, Weight::Destroy, enabled)
                .into_any_element(),
            Reset::Confirming => div().into_any_element(),
            Reset::Running { .. } => kit.detail(crate::status::RESETTING).into_any_element(),
        };
        let mut rows = vec![
            kit.setting(
                "Start over",
                Some(
                    "Remove settings and downloaded engines and models, then download Parakeet again. Your Shows stay."
                        .into(),
                ),
                control,
            )
            .into_any_element(),
        ];
        if matches!(self.reset, Reset::Confirming) {
            rows.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .py(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(200.0))
                            .text_size(px(13.0))
                            .child("Speakeasy forgets your settings, words, and presets."),
                    )
                    .child(ui.button("Cancel", Action::CancelReset, Weight::Quiet))
                    .child(ui.button_if(
                        "Delete settings and downloads",
                        Action::ConfirmReset,
                        Weight::ConfirmDestroy,
                        enabled,
                    ))
                    .into_any_element(),
            );
        }
        kit.group(rows)
    }
}

fn permission_row(
    ui: &Ui<'_>,
    name: &'static str,
    state: Permission,
    help: permissions::Help,
) -> Div {
    let palette = ui.palette;
    let blocked = matches!(state, Permission::NotGranted | Permission::Restricted);
    let status = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .text_size(px(13.0))
        .text_color(rgb(if blocked { palette.warn } else { palette.muted }))
        .when(state == Permission::Granted, |status| {
            status.child(icon(icons::CHECK, 14.0, palette.lamp))
        })
        .child(permissions::description(state));
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .min_h(px(52.0))
        .px(px(16.0))
        .py(px(10.0))
        .child(div().flex_1().child(name))
        .child(status)
        .when(blocked && !is_demo(ui.cx), |row| {
            row.child(ui.button(
                permissions::OPEN,
                Action::PermissionHelp(help),
                Weight::Secondary,
            ))
        })
}
