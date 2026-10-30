# Release notes

Speakeasy 0.7.0 makes a Show harder to lose and easier to start: the shortcut finishes it on
release, Escape alone stays with your app, an interruption saves it rather than discarding it, and a
Show whose narration fails is still saved. The tray can start a Show and copy or open the last one,
and Windows can start Speakeasy when you sign in.

- **Finishing and cancelling a Show.** Pressing the shortcut again finishes a Show when you release
  it, so holding the shortcut and tapping <kbd>Escape</kbd> cancels it instead. <kbd>Escape</kbd>
  alone stays with your app during a Show: cancel with the shortcut and <kbd>Escape</kbd>, or
  **Cancel Show** in the tray. A Show that is finishing is saved even if you press
  <kbd>Escape</kbd>. Pressing another shortcut while you hold Speakeasy's, locking the screen, or
  sleeping finishes the Show rather than discarding it. Dictation's gestures are unchanged.
- **Shows from the tray.** On Windows and Mac, **Show screen** and **Show area…** start a Show of
  the display under the pointer or of an area you drag out, for a keyboard without the shortcut,
  such as a Mac keyboard with no Fn key. **Copy last Show** copies the latest Show's `summary.md`
  again as it reads now, edits included, and **Open last Show** opens its folder. Speakeasy
  remembers only that one Show, and only until it quits; a finished Show that cannot be copied stays
  saved, and the pill points you to Copy last Show.
- **A Show records only what it names.** <kbd>W</kbd> over no window it can record, such as the
  desktop or the taskbar, and <kbd>A</kbd> finished before you drag out an area, or clicked instead,
  cancel the Show and say why. A recorded window that closes, is minimized or hidden, moves to
  another desktop, or goes off screen ends the Show with what it recorded until then, as does screen
  capture that stops early; the pill and the summary say why and when.
- **Narration that fails.** When the microphone stops or your speech cannot be transcribed, the Show
  is saved without narration rather than lost: the pill says **Show copied without narration** and
  points to Settings for what went wrong, the summary tells your agent why, and no audio is kept.
- **Summaries for agents.** `summary.md` asks your agent to open the contact sheets and the frames
  that matter before acting, to trust the screen's spelling of a name over speech recognition, and
  to cite frames as #N and marks by their circled numbers. Every path in it is a code span, so a
  folder with spaces reads unambiguously, and a Show's files reach the disk before it appears.
- **Ink and keyframes.** One-stroke arrows and loops drawn inside longer strokes are recognized, and
  a crossed stroke such as an X reads as neither. Ink pairs with the sentence you say around it,
  across pauses to draw and hesitations. A click's keyframe shows its result once the change settles
  and counts only a change that starts within 500 ms, and a quick click keeps its own keyframe.
  Clicks are marked with a thin ring and dot edged in white, which leave the control legible. A
  window Show that is resized is measured at its new size.
- **Clipboard.** A copied Show summary stays out of Windows clipboard history and cloud clipboard,
  and out of Universal Clipboard and clipboard managers that honor transient items on Mac, as pasted
  dictation does.
- **Start at login.** On Windows, **Start at login** in Settings, off by default, opens Speakeasy in
  the tray when you sign in. If Windows' Startup apps page has turned it off, Settings says so
  beside the switch.
- **Settings and the command line.** Settings explains each Show key, drawing, pins, and finishing.
  A settings file that fails to read names the key at fault, without its value.
  `speakeasy --version` prints the version. A rejected command line, or a launch that fails before
  its windows open, is reported on the terminal that ran it, with status 2 or 1; only a desktop
  launch shows a dialog. `--config` and `--out` must name a path.
- **Speech.** A transcription that fails is retried once on a fresh engine, so an engine that died
  while idle no longer costs the recording. An engine that dies names the cause and keeps its
  remedy, and the local engine starts on Linux installs without a CA bundle.
- **Linux.** libei capability lists end as libei reads them, and X11 paste rechecks held keys right
  before it pastes.
- **Under the hood.** Code, tests, and documentation across the tree are polished.

Dictation, models, filler removal, and sound cues are otherwise unchanged. Recordings retain the
five-minute limit. Audio and recognition stay local, with no dictation history or uploads; Shows
stay private and are deleted after seven days unless a `.pinned` file keeps them.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
