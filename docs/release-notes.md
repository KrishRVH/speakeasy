# Release notes

Speakeasy 0.3.5 lays the groundwork for Shows: narrated screen recordings, with ink drawn over the
screen, that a coding agent can read.

- `speakeasy show demo` writes a scripted Show as a bundle: annotated keyframes, a contact sheet,
  `summary.md`, and `show.json`. It then prints the summary. Keyframes follow the ink, clicks,
  screen changes, and pointing words such as "look" and "now", and each keyframe carries the
  narration that belongs to it. Recording a Show of your own screen is not available yet.
- On Windows, `speakeasy --help` and `speakeasy show` print to the terminal that started them.

Dictation, existing settings, engines, models, filler removal, and gestures are unchanged.
Recordings retain the five-minute limit. Audio and recognition stay local, with no transcript
history or uploads.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, clipboard/editor, and compositor acceptance remain separate; see
[architecture](architecture.md) and [performance](performance.md).
