# Release notes

Speakeasy 0.11.1 sends a dictation with Return, puts your clipboard back after a paste, and keeps
your words when the microphone drops out.

- **Return to send.** On Windows and Mac, press Enter or Return during the hold to finish the
  dictation, type it, and press Return after it, so a chat or an agent's prompt sends it. Return
  with the shortcut up reaches your app as usual.
- **Your clipboard comes back.** On Windows and Mac, a paste puts back what the clipboard held half
  a second later, unless something else changed it in the meantime. The switch that once read **Keep
  clipboard** is now **Type instead of paste**; it still types the text without touching the
  clipboard.
- **Back-to-back dictations spaced.** On Windows and Mac, a dictation sent straight after the last
  one into the same field, with no typing, clicking, or pointer movement in between, starts with a
  space.
- **Spoken symbols.** In Prose, “new line”, “new paragraph”, “underscore”, “open paren”, “backtick”,
  and similar phrases write their symbol. Turn off **Spoken symbols** under Writing to keep the
  words.
- **Right Ctrl or F13 on Windows.** Choose either as the shortcut when another app already uses
  Ctrl+Win.
- **Dictation through a lost microphone.** Audio glitches and rerouting now leave only a gap. If the
  microphone disconnects or stops sending audio after you spoke, Speakeasy keeps the words for
  **Paste last dictation** and says how to paste them. A microphone that sends nothing for two
  seconds gets an error, and when the microphone you chose isn't connected, Speakeasy records from
  the system default and says so.
- **Stays awake.** On Windows and Mac the computer stays awake from the start of a recording until
  its words are delivered, and a Show also keeps the display on.
- **Practice to correction.** Practice a word Speakeasy keeps mishearing, then choose it in the
  result: a correction opens with what you said, so you only type what to write.
- **Scripts can read dictations.** Once you turn on **Scripts can read their dictations**,
  `speakeasy dictation start --print` and `dictation stop TOKEN --print` return that dictation's
  words to the script that started it, and only to it.
- **Shows for agents.** `speakeasy show wait` waits for the next Show you save. Summaries name the
  Show ID and, on Windows with WSL, the folder as WSL sees it, and Speakeasy's MCP tools work from
  WSL. Narration without timing gets its own summary section.
- **Whisper.** Whisper takes your recognition hints as its prompt and drops invented credits such as
  “Thanks for watching!” from brief, near-silent recordings.
- **Mac.** The pill says when another app's Secure Keyboard Entry keeps Escape, Return, and the hold
  keys from reaching Speakeasy, and it follows the pointer onto a screen's top row. Local engines
  keep ggml's Metal tensor kernels off; on M5-class Macs they returned no speech or garbled text.
- **Linux.** Speakeasy opts out of PipeWire's realtime budget, which could get it killed seconds
  after the microphone opened, and treats an empty `WAYLAND_DISPLAY` as no Wayland session.
- **Windows.** The installer names KrishRVH as publisher, so winget no longer offers an unrelated
  package as an upgrade for Speakeasy.
- **Clearer wording.** The tray names the speech model and adds “Open Settings” only when Settings
  can fix the problem. Settings reads “Setting up…” during first setup, errors quote the setting to
  change, a failed paste leads with how to get your words back, and the Shows window says when it
  cannot read the Shows folder.
- **Smaller fixes.** Settings keeps keyboard focus on controls whose action changes. A settings file
  behind an unreadable folder is reported instead of treated as a fresh install. A preset is
  described by the model it actually runs, and `show wait` fails for a damaged Show as
  `show inspect` does.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows expire
after seven days unless kept; keeping a Show never extends its audio retention.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
