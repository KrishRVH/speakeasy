# Release notes

<https://github.com/user-attachments/assets/d0b50077-69d2-487a-b177-7a9b5ed1f6e9>

Speakeasy 0.6.0 lets a Show record one window or an area you drag out, not only the whole display,
numbers every mark and pin so you and your coding agent can refer to them, and makes the shortcut,
the clipboard, and Settings more dependable.

- **Window and area Shows.** While holding the shortcut, <kbd>W</kbd> records the window under the
  pointer, cropped from its display and followed as it moves, and <kbd>A</kbd> shades the display
  until you drag out the area to record. <kbd>S</kbd> still records the whole display. Each keeps
  what you said since you pressed the shortcut, and a thin red line marks the window or area while
  it records; keyframes show only what it covers.
- **Numbered pins.** Double-tap <kbd>Shift</kbd> during a Show to drop a pin where the pointer is,
  for the spot you mean without drawing. Taps that draw, click, or join another modifier never pin,
  and Shift still reaches your apps, so their own double-Shift shortcuts keep working.
- **Numbered marks.** Marks and pins share one sequence, ① ② ③, in the order you made them. Each
  number sits beside its mark on screen while the ink shows and on the keyframes, and the summary
  names them by number, for example `[① circled, ② drew an arrow]`. `show.json` moves to format
  version 2, which records the pins and each mark's number.
- **Shortcut.** A hold whose release the system never delivered ends at that release, so the next
  Space, S, W, or A reaches your app and the next press starts dictation; S, W, and A ask for a Show
  only while the shortcut is held. Windows keeps its keyboard hook running through logon and with
  Remote Desktop Services off. When macOS pauses the key tap, Speakeasy turns it back on and cancels
  only a hold whose keys went unseen, so a dictation already transcribing still pastes. On Mac,
  Command, Option, or Control during a Fn hold cancels dictation as any other key does, and a key
  event another app posts no longer ends a Fn hold early.
- **Clipboard.** Pasted dictation stays out of Windows clipboard history and cloud clipboard, and
  out of Universal Clipboard and clipboard managers that honor transient items on Mac. A copied Show
  summary stays ordinary clipboard text.
- **Settings.** **Resume dictation** resumes the saved settings, and reads **Save and resume** when
  you have edits. **Unsaved changes** stays beside any other notice until you save or revert. While
  a resume checks the model, Settings and the tray show it loading rather than unavailable, and once
  Quit begins they say **Quitting**. The example settings file now holds what a first launch starts
  from: Parakeet with automatic setup.
- **Shows.** On Windows the overlay covers the display edge to edge, so ink lines up with the window
  frame. A Show whose overlay cannot cover its display, or whose recorder fails, hides the overlay
  and reports the failure. Footage ends as soon as you finish, and a stroke that joins a faded mark
  gets its own number.
- **Linux.** Paste through libei sends Ctrl+V without Shift, except for terminal paste, wherever the
  keymap puts Num Lock. Speakeasy checks for libei before asking for Wayland permissions, ends setup
  when the desktop closes a session during consent, and fails only the clipboard transfer the portal
  refuses. Manual paste keeps its desktop bindings across keyboard map changes, and X11 reports
  ready only after insertion is set up.
- **Privacy.** Recorded audio is zeroed wherever it is dropped, through writes the compiler keeps.
- **Under the hood.** Code across the tree is reworked into idiomatic Rust without changing
  behavior. Command-line paths need not be valid Unicode. Windows contains the engine through the
  handle Speakeasy spawned it with, and a panic in its session-change callback stays out of the OS.
  Settings views share one microphone scan, which Quit waits for.

Dictation, models, filler removal, gestures, and sound cues are otherwise unchanged. Recordings
retain the five-minute limit. Audio and recognition stay local, with no dictation history or
uploads; Shows stay private and are deleted after seven days unless a `.pinned` file keeps them.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
