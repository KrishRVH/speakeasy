# Release notes

Speakeasy 0.3.2 brings the codebase to an idiomatic Rust standard, gives every thread, process, and
native event loop one explicit owner, and fixes small Settings, pill, Linux, and error-reporting
issues.

- Keep keyboard focus on a Settings button when its label changes, such as a toggle switching
  between On and Off. A restarted setup shows its own progress rather than the paused attempt's, and
  Settings shows its status from the first frame.
- Show the newest audio level at the center of the pill's grille for every bar count, including
  while the capsule resizes.
- Report a failed capture thread as "Microphone stopped unexpectedly. Try recording again." instead
  of leaving the session finishing. Zero unencoded recording audio whenever its buffer is released.
- On Linux, let specific desktop-access errors, such as a keyboard mapping change, reach Settings
  instead of a generic stop message. Wayland manual paste asks you to release the shortcut and
  paste, and `--help` lists `--toggle` and `--cancel`. Error dialogs include their cause.
- Own background threads, native input monitors, and engine processes through shared helpers that
  join, wake, or kill and reap them before replacement. Split Settings, the pill, and the tray into
  focused modules; share pill geometry, shortcuts, and service state from the platform crate; and
  use typed AppKit calls on macOS. Windows and macOS insertion share one pre-commit check.

Windows x64 and Apple silicon macOS zip packages, plus Linux x86_64 AppImage and bundled tar
packages, are attached with SHA-256 checksums. Existing settings, engines, models, dictation
gestures, recognition policy, and the five-minute recording limit are preserved.

Linux remains experimental. This release changes native adapters on all three platforms; CI builds,
lints, and tests them natively and runs owned-window rendering checks, while live microphone,
keyboard-hook, clipboard/editor, and compositor acceptance remain separate. Forced native
termination retains synchronous cleanup. Current contracts and measurement limits are in
[architecture](architecture.md) and [performance](performance.md).
