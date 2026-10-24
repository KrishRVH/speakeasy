# Release notes

Speakeasy 0.4.0 makes Shows easier to start and draw in, keeps Settings' controls and status in
view, and makes many more of its errors say what to do next.

- **Shows.** On Windows, <kbd>Shift</kbd> draws even while Ctrl+Win is still held. S starts a Show
  from any hold, including one waiting for the microphone right after a dictation and one already
  locked hands-free. A click that opens a new screen right after a drawing gets a keyframe of its
  own, a click made while drawing is reported on the drawing's keyframe, and `--help` says how to
  start a Show.
- **Settings.** The buttons and the status line stay at the bottom while the controls scroll, and
  the window opens tall enough for every control where the display has room. Only problems show in
  the attention color. A fresh install shows Parakeet, and the button that restarts dictation reads
  **Resume dictation**, as the tray and every remedy say.
- **Tray and status.** Before setup finishes, the tray and Settings say Speakeasy is not set up
  rather than offering a Resume that could only fail. During a Show they say it is recording or
  preparing the Show.
- **The pill.** The clock and its mark sit evenly at the capsule's end, and a message longer than
  two lines ends with an ellipsis above **Details in Settings**.
- **Errors.** Microphone, engine, setup, settings, and Show-saving failures say what happened and
  what to do: a full disk asks for space, a failed write names its folder or file, and a download
  the server refuses names its answer. An image or files copied over dictated text before paste
  report that the clipboard changed, on Windows, macOS, and Linux X11. On Wayland, a clipboard
  another app takes before paste says so with a remedy where it said Cancelled. Linux errors that
  already say what to do are no longer wrapped in general advice.
- **Setup.** A setup paused during the model download resumes without downloading the engine builds
  it already checked, and the engine check has its own step label.
- **Reliability.** The running instance keeps answering `--toggle`, `--cancel`, and relaunches after
  a failed connection. On Windows, Speakeasy's hidden lifecycle window stays invisible when a
  utility shows hidden windows.
- **Under the hood.** The Rust toolchain, crates, and build tools move to their latest compatible
  releases. README says that on Windows, streaming and screen-sharing apps don't see Speakeasy's
  windows or a Show's ink while it records, and lists the Linux build packages.

Dictation, models, filler removal, and gestures are otherwise unchanged. Recordings retain the
five-minute limit. Audio and recognition stay local, with no dictation history or uploads; Shows
stay private and expire after seven days unless pinned.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
