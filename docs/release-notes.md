# Release notes

Speakeasy 0.12.0 animates the pill at your display's refresh rate, lets Windows rest while it sits
hidden, keeps voice with kept Shows, and gives agents a Show's whole narration.

- **Smoother pill.** Since 0.10.0, the GPUI version Speakeasy draws with has held windows that never
  take focus to 30 fps, so the pill and the Show overlay animated at 30 fps on every platform. They
  now follow the display's refresh rate up to 240 fps, or 30 fps in battery saver: about 240 frames
  a second on a 400 Hz Windows display, at about 0.16 ms of drawing each.
- **Idle on Windows.** While every Speakeasy window is hidden, Speakeasy no longer wakes on each
  screen refresh, about 1,000 times a second on a 400 Hz display. It now wakes about eight times a
  second, with no measurable CPU.
- **Faster.** Composing a finished Show takes about 40% less time. Windows Show video holds its
  frame rate when capture work runs long, which used to drop it below 30 fps. On Mac, a chosen
  microphone is found without reading every audio device's formats at each press.
- **Kept Shows keep their voice.** Saved voice now lives and expires with its Show, so **Keep Show**
  keeps it too; before, voice expired seven days after recording even in a kept Show. **Delete
  Show…** removes the bundle and leaves pending recovery audio from an interrupted recording until
  it expires or `speakeasy show discard` removes it. An interrupted Show left by 0.11.1 recovers by
  transcribing its kept audio again.
- **Whole narration for agents.** MCP `show_read` returns the whole recorded narration as timed
  rows, the clauses that review and captions use, instead of stopping at 512 words. Tool errors name
  their cause, and `show_list` names roots it cannot list in `unavailableRoots` while listing the
  rest.
- **Changed replies.** `show list`, `show inspect`, and `show wait` JSON and MCP Show metadata
  report a Show Speakeasy cannot read as `status: "unreadable"` with one `error` string, in place of
  `damaged`, `unsupported`, and the `problems` list. MCP narration carries `rows` of `atMs` and
  `text` in place of `words`, `totalWords`, and `truncated`, and untimed narration carries `text` in
  place of `excerpt`. MCP image metadata names times `sampledMs` and `requestedMs`, as `show frame`
  does, and drops `sampleAfterRequest`. `speakeasy-cli` exits 2 for its own failures, such as a
  missing `speakeasy` beside it.
- **Line breaks that stay put.** With **Type instead of paste**, Windows and Mac type each line
  break as Shift+Enter, so “new line” no longer sends a chat message or accepts an editor
  suggestion. Return during the hold still sends.
- **Keys and clipboard.** On Windows and Mac, Escape after Speakeasy copied a dictation but before
  it pasted puts your clipboard back. S, W, A, V, Space, and Return act only when pressed during the
  hold: a key already down or repeating no longer acts, and one pressed during a hold stays out of
  your app until you release it.
- **Presets and setup.** **Use** in Settings asks you to finish a recording or Show instead of
  abandoning it. The setup guide keeps offering setup while the saved model is missing instead of
  calling speech ready. Saving settings replaces a symlinked `settings.json` with a regular file.
- **Transcripts.** Whisper keeps a word whole when its segments wrap inside it. Spoken symbols treat
  CJK and other Unicode full stops as sentence ends and leave “forward, slash prices” as words, and
  removing a final “um” no longer leaves “;.”. Show captions end at a lone closing quote or “。”.
- **Show fixes.** A Windows window Show waits out a moment of unreadable window bounds instead of
  ending. A pin lands where the pointer was when your taps ended, the screen recording stops at the
  five-minute limit, and a narration retry that hears no words keeps the retry. Phrase searches and
  `show video --at` say when you asked past the recorded timeline.
- **Linux.** A paste into a GNOME app no longer stalls while Speakeasy has nothing to offer the
  clipboard. With libei 1.4 or newer, a resumed Wayland keyboard waits to learn which modifiers are
  held before pasting. An X11 paste that may have landed asks you to check the editor instead of
  pasting again. An event left over from before a Wayland window was hidden no longer closes the
  window or drops the connection.
- **Smaller fixes.** `speakeasy dictation stop TOKEN --print` returns dictations of any length, and
  **Copy last Show** in the tray plays the copy chord.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows and their
saved voice expire after seven days unless kept; pending recovery audio always expires seven days
after recording.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
