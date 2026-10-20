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
- Text left on the clipboard for you to paste is a notice that says how, no longer an error.
- Settings files from other releases load. Saving writes only what you changed, so hand edits stay,
  and a theme or reduced-motion change no longer stops a recording.
- On Linux, Settings' **Insert** control chooses how text arrives, and Settings shows the exact
  desktop command to bind.
- On Linux, the first paste after starting no longer reports that the clipboard changed, and on X11
  a paste no longer stops dictation with a keyboard mapping error.
- On Windows and Mac, the tray's **Start dictation** waits until desktop input is ready.
- Windows no longer needs the Visual C++ Redistributable, and the Linux app is smaller.
- Mac permission errors explain how to restore access that already looks granted.
- Shift no longer cancels dictation when pressed during a Windows hold.
- `--demo` previews a Show, with ink drawn and fading on screen on Windows and Mac.
- Dependencies and tools are updated to their latest stable releases, except libloading, tiny-skia,
  x11rb, and zbus, which match the versions GPUI uses.

Dictation, existing settings, models, filler removal, and gestures are otherwise unchanged.
Recordings retain the five-minute limit. Audio and recognition stay local, with no dictation history
or uploads.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
