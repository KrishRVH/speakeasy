# Release notes

<https://github.com/user-attachments/assets/3ccaba96-808a-4d01-891f-db7338d519c4>

Speakeasy 0.6.0 lets a Show record one window or an area you drag out, not only the whole screen,
and numbers every mark and pin so you and your coding agent can refer to them.

- **Window and area Shows.** While holding the shortcut, <kbd>W</kbd> records the window under the
  pointer, cropped from its display and followed as it moves, and <kbd>A</kbd> shades the display
  until you drag out the area to record. <kbd>S</kbd> still records the whole display. Each keeps
  what you said since you pressed the shortcut, and a thin red line marks the window or area while
  it records; keyframes show only what it covers.
- **Numbered pins.** Double-tap <kbd>Shift</kbd> during a Show to drop a pin where the pointer is,
  for the spot you mean without drawing. Shift still reaches your apps, so their own double-Shift
  shortcuts keep working.
- **Numbered marks.** Marks and pins share one sequence, ① ② ③, in the order you made them. Each
  number sits beside its mark on screen while the ink shows and on the keyframes, and the summary
  names them by number, for example `[① circled, ② drew an arrow]`. `show.json` records the pins and
  each mark's number.

Dictation, models, filler removal, gestures, and sound cues are unchanged. Recordings retain the
five-minute limit. Audio and recognition stay local, with no dictation history or uploads; Shows
stay private and are deleted after seven days unless a `.pinned` file keeps them.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. Verification runs locally. Live
microphone, keyboard-hook, screen capture, clipboard/editor, and compositor acceptance remain
separate; see [architecture](https://github.com/KrishRVH/speakeasy/blob/main/docs/architecture.md)
and [performance](https://github.com/KrishRVH/speakeasy/blob/main/docs/performance.md).
