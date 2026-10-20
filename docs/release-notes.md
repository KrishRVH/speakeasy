# Release notes

Speakeasy 0.3.6 records Shows: narrated screen recordings, with ink drawn over the screen, that a
coding agent can read.

- Press <kbd>S</kbd> while holding the dictation shortcut to make the recording a Show. Speakeasy
  records the display under the pointer along with your voice; hold <kbd>Shift</kbd> to draw ink
  that fades as you go, and click through as usual. Press the shortcut again to finish, then paste
  the copied summary into your coding agent.
- Speakeasy's own windows never appear in a Show. Shows are written to the `shows` folder beside the
  engine and model, readable only by you, and deleted after seven days unless a file named `.pinned`
  in their folder keeps them. Cancelling leaves nothing behind.
- Shows record on Windows, and experimentally on Mac, which asks for screen recording access on the
  first Show. Linux records Shows in a later release.
- A finished Show is copied within a fraction of a second. Recording costs a few percent of one CPU
  core, ink holds GPU memory only while it shows, and an idle Speakeasy uses what it did before.
- New installs set up NeMo-Speech.cpp 0.2.0, which recognizes speech as 0.1.0 did. Existing installs
  keep their engine.
- Shift no longer cancels dictation when pressed during a Windows hold.
- `--demo` previews a Show, with ink drawn and fading on screen on Windows and Mac.
- Dependencies and tools are updated to their latest stable releases.

Dictation, existing settings, models, filler removal, and gestures are otherwise unchanged.
Recordings retain the five-minute limit. Audio and recognition stay local, with no dictation history
or uploads.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](architecture.md) and [performance](performance.md).
