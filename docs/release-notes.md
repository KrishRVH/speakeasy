# Release notes

Speakeasy 0.12.1 lets you record any shortcut to talk, adds one-press shortcuts for hands-free and
Shows, and makes removed presets, corrections, and Shows easier to manage.

- **Any shortcut.** On Windows and Mac, Settings records any keys for hold to talk, beside the
  offered Right Ctrl, F13, Right Option, and Right Command. Character keys show what your keyboard
  layout prints. Settings refuses keys that would type, including Windows AltGr chords and Mac
  Option characters, and says why on the row. `settings.json` names a shortcut by its keys, such as
  `"control+windows"`; `"control_windows"` from earlier versions still loads.
- **One-press hands-free and Shows.** Optional shortcuts start hands-free or a screen, window, or
  area Show with one press and finish it with the next. During a hold or the wait for a second tap,
  the hands-free shortcut locks hands-free, and a Show shortcut turns the dictation into its Show,
  as S, W, or A does.
- **Relaxed double-tap.** A press released within 300 ms, up from 220 ms, counts as a tap, so an
  unhurried double-tap locks hands-free.
- **Mac Fn.** When macOS also acts on Fn (**System Settings → Keyboard → Press 🌐 key to**),
  Settings warns with a button to Keyboard settings, and the pill notes it as a recording opens. The
  Globe key's own key event no longer interrupts an Fn hold.
- **Notes under the pill.** Secure input and fallback-microphone notes open as a line under the
  slot, so the grille, lock, and timer stay visible, and each note gets its full time. The Secure
  input note names the app that turned it on.
- **Undo and quick delete.** Deleting a preset and removing a phrase correction each offer Undo.
  Each saved Show in the Shows list has a red trash button that opens the delete confirmation.
- **Clearer messages.** Undelivered words and uncopied Shows lead with the remedy. The tray status
  names what failed, such as “Microphone unavailable”. A settings file Speakeasy can't read shows
  its parse error and opens its folder. A preset chosen from the tray is tagged **This session** in
  Settings, with **Keep using it**. **Allow expiry** is now **Stop keeping**, **All retained
  frames** is **Every saved frame**, and the setup guide's **Cancel** is **Pause download**. Errors
  from the app, CLI, and MCP say “can't” and “couldn't”.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows and their
saved audio expire after seven days unless kept; pending recovery audio always expires seven days
after recording.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
