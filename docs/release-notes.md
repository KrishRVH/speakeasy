# Release notes

Speakeasy 0.3.7 shows how to draw in a Show and makes more of its errors say what to do next.

- When a Show starts recording, the pill reads **Hold Shift to draw** for three seconds before the
  elapsed time returns, and Settings lists the gesture beside the dictation shortcut on Windows and
  Mac. To draw, release the shortcut, then hold <kbd>Shift</kbd> and move the pointer without
  clicking.
- `--demo` previews the drawing hint and the disconnected-microphone error that Speakeasy reports.
  `speakeasy show demo` reports bugs in a made-up settings app rather than in one named Speakeasy.
- Settings lists every microphone it can name when one cannot be read, where it listed none, and
  clears a failed model load once the model recovers.
- Microphone and local engine errors say how to recover, and an engine that stops mid-reply reports
  that it stopped.
- On Linux X11, an error names the dictation or cancel shortcut that another app already holds. On
  Wayland, losing desktop input while typing stops desktop access with steps to restore it, where
  each later paste failed with the wrong advice.
- The README walks through every feature with screenshots.

Dictation, settings, models, filler removal, gestures, and Shows are otherwise unchanged. Recordings
retain the five-minute limit. Audio and recognition stay local, with no dictation history or
uploads.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
