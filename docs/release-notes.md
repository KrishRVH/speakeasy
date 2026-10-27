# Release notes

<https://github.com/user-attachments/assets/3ccaba96-808a-4d01-891f-db7338d519c4>

Speakeasy 0.5.0 confirms dictation and Shows with soft sound cues, is free software under the MIT
License with notices for the Rust crates in every package, and opens its README with a short tour of
Shows.

- **Sounds.** Soft kalimba notes mark the microphone listening, a recording finishing, including at
  the five-minute limit, a cancel, a Show starting, and text or a Show copied. Speakeasy listens
  beneath its own cues, so a recording that hears only a cue sends nothing, while a word said over
  one still counts. Turn them off with **Play sounds** in Settings.
- **License.** Speakeasy is open source under the MIT License. Every package includes `LICENSE` and
  `THIRD-PARTY-NOTICES.txt`, which lists the Rust crates built into that platform's app with their
  license texts, beside the font license and the Show notice.
- **Shows explained.** The README opens with a 29-second video of a Show: hold the shortcut, tap S,
  talk and draw over the app, and paste the keyframes and summary into a coding agent.
- **Finer ink.** Show ink draws a third thinner, on screen and in keyframes, so a mark leaves the
  text it points at readable while still standing out on a contact sheet.
- **Linux.** With an empty or relative `HOME` and no absolute `XDG_STATE_HOME`, Speakeasy keeps the
  desktop portal's restore token out of the working directory, and the portal asks for consent on
  each launch.
- **Privacy.** Diagnostic output never includes screen pixels, narration, or a Show's spoken cue
  words.
- **Under the hood.** The pinned nightly Rust toolchain moves to 2026-10-26 with stricter lints, and
  code, tests, and comments across the tree are tightened without changing behavior.

Dictation, models, filler removal, gestures, and Shows are otherwise unchanged. Recordings retain
the five-minute limit. Audio and recognition stay local, with no dictation history or uploads; Shows
stay private and expire after seven days unless pinned.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
