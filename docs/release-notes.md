# Release notes

Speakeasy 0.4.1 makes a Show cover the whole recording, keeps the recording clock on time, and keeps
saved settings when dictation cannot start.

- **Shows.** A Show keeps up to 36 keyframes spread across the recording, where it could stop
  partway through, and its last screen gets a keyframe unless drawings fill all 36. Each contact
  sheet holds at most six frames, a longer Show divides its frames evenly across sheets, and the
  summary names every sheet and the frames on it. Publishing loads one sheet's frames at a time, so
  a long Show's memory use stays bounded.
- **The pill.** The recording clock ticks on the recording's whole seconds, so it no longer lags or
  skips a second while the microphone is silent, and the countdown to the five-minute limit starts
  on time.
- **Settings.** When dictation cannot start after a save, Speakeasy keeps the saved settings, the
  status says dictation is paused, and **Resume dictation** starts what was saved.
- **Tray preview.** `--demo-tray` leaves the user's settings folder alone, where it could stop the
  real app from showing its first-hide hint.
- **Errors.** When Speakeasy cannot lock its instance file at startup, the error names the system's
  reason.
- **Under the hood.** Windows and macOS builds leave out networking code only the Linux adapters
  use, and comments, tests, and docs across the tree are tightened.

Dictation, models, filler removal, and gestures are otherwise unchanged. Recordings retain the
five-minute limit. Audio and recognition stay local, with no dictation history or uploads; Shows
stay private and expire after seven days unless pinned.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
