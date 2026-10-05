# Release notes

Speakeasy 0.3.4 reduces application work on the path from finishing a recording to recognition.

- Sealed audio reaches the speech worker before native microphone teardown finishes. A replacement
  recording still waits for the previous microphone to finish cleanup.
- Speech windows are classified while recording. Sealing uses the accumulated classification,
  preserving the recording and audible-speech gates, quiet-edge trimming, word padding, and interior
  pauses.
- Native insertion rechecks released shortcut modifiers every 2 ms.

Existing settings, engines, models, filler removal, and dictation gestures are preserved. Recordings
retain the five-minute limit. Audio and recognition stay local, with no transcript history or
uploads.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification and owned-window rendering
checks run locally. Live microphone, keyboard-hook, clipboard/editor, and compositor acceptance
remain separate; see [architecture](architecture.md) and [performance](performance.md). Native
end-to-end latency gains remain unmeasured.
