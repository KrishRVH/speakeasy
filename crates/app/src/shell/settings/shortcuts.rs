//! Shortcut editing: presets for hold to talk, and a recording that sets any shortcut.

use gpui::{Context, Task};
use speakeasy_platform::{Binding, NativeShortcut, Recorded, Refusal};

use super::{Services, Settings};
use crate::gpui_ext::EntityUpdate;

/// The hold-to-talk shortcuts the menu offers; Record sets any other.
pub(super) const PRESETS: &[NativeShortcut] = cfg_select! {
    target_os = "macos" => &[
        NativeShortcut::FUNCTION,
        NativeShortcut::RIGHT_OPTION,
        NativeShortcut::RIGHT_COMMAND,
        NativeShortcut::F13,
    ],
    _ => &[
        NativeShortcut::CONTROL_WINDOWS,
        NativeShortcut::RIGHT_CONTROL,
        NativeShortcut::F13,
    ],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Preset(NativeShortcut),
    Record(Binding),
    Cancel,
    Remove(speakeasy_platform::Toggle),
}

#[derive(Default)]
pub(super) struct Editor {
    recording: Option<Recording>,
    /// Keys a recording chose that Settings refused, shown on their row until the next change.
    refused: Option<(Binding, NativeShortcut, Refusal)>,
}

struct Recording {
    binding: Binding,
    holding: Option<NativeShortcut>,
    /// Dropping it drops the receiver, which ends the recording before the next key.
    _listen: Task<()>,
}

impl Editor {
    pub(super) fn is_recording(&self, binding: Binding) -> bool {
        self.recording
            .as_ref()
            .is_some_and(|recording| recording.binding == binding)
    }

    /// The keys held so far during a recording.
    pub(super) fn holding(&self) -> Option<NativeShortcut> {
        self.recording
            .as_ref()
            .and_then(|recording| recording.holding)
    }

    pub(super) fn refused(&self, binding: Binding) -> Option<(NativeShortcut, Refusal)> {
        self.refused
            .filter(|(refused, ..)| *refused == binding)
            .map(|(_, keys, refusal)| (keys, refusal))
    }

    pub(super) fn stop(&mut self) {
        self.recording = None;
    }
}

impl Settings {
    pub(super) const fn can_record_shortcut(&self) -> bool {
        self.presentation.can_record_shortcut
    }

    pub(super) fn edit_shortcut(&mut self, edit: Edit, cx: &mut Context<'_, Self>) {
        // Any edit ends a recording; a saved change restarts the input monitor, which would drop
        // its keys anyway.
        self.shortcuts.stop();
        match edit {
            Edit::Preset(shortcut) => self.bind(Binding::Talk, Some(shortcut), cx),
            Edit::Record(binding) => self.record_shortcut(binding, cx),
            Edit::Cancel => {},
            Edit::Remove(toggle) => self.bind(Binding::Toggle(toggle), None, cx),
        }
    }

    fn record_shortcut(&mut self, binding: Binding, cx: &Context<'_, Self>) {
        self.shortcuts.refused = None;
        if !self.can_record_shortcut() {
            return;
        }
        let Some(results) = cx.global::<Services>().record_shortcut() else {
            return;
        };
        let listen = cx.spawn(async move |this, cx| {
            while let Ok(recorded) = results.recv().await {
                this.update_if_alive(cx, |view, cx| view.recorded(recorded, cx));
            }
            // The input monitor restarted or stopped, so no key will reach this recording.
            this.update_if_alive(cx, |view, cx| {
                view.shortcuts.stop();
                cx.notify();
            });
        });
        self.shortcuts.recording = Some(Recording {
            binding,
            holding: None,
            _listen: listen,
        });
    }

    fn recorded(&mut self, recorded: Recorded, cx: &mut Context<'_, Self>) {
        let Some(recording) = &mut self.shortcuts.recording else {
            return;
        };
        let binding = recording.binding;
        match recorded {
            Recorded::Holding(keys) => recording.holding = keys,
            Recorded::Cancelled => self.shortcuts.recording = None,
            Recorded::Chosen(keys) => {
                self.shortcuts.recording = None;
                self.bind(binding, Some(keys), cx);
            },
        }
        cx.notify();
    }

    /// Sets or clears a binding's keys, unless the other shortcuts refuse them.
    fn bind(&mut self, binding: Binding, keys: Option<NativeShortcut>, cx: &mut Context<'_, Self>) {
        let mut shortcuts = self.config.shortcuts();
        if let Some(keys) = keys
            && let Some(refusal) = shortcuts.refusal(binding, keys)
        {
            self.shortcuts.refused = Some((binding, keys, refusal));
            return;
        }
        match (binding, keys) {
            (Binding::Talk, Some(keys)) => shortcuts.talk = keys,
            (Binding::Talk, None) => return,
            (Binding::Toggle(toggle), keys) => shortcuts.toggles.set(toggle, keys),
        }
        self.shortcuts.refused = None;
        self.config.set_shortcuts(shortcuts);
        self.edited(cx);
    }
}
