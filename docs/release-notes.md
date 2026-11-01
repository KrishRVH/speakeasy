# Release notes

Speakeasy 0.8.0 adds Show review and recovery, voiced and annotated video exports, more everyday
dictation controls, and explicit agent integrations.

**Breaking changes:** Until 1.0.0, Speakeasy supports its current formats and settings only. Shows
use `show.json` format 3 and the current recording and recovery formats; earlier formats are
unsupported. Settings use `engine: "nemo"` and one `text_delivery` choice; old engine names and
delivery keys are rejected. Omitted engine and language settings now mean `nemo` and `auto`. There
are no migration or compatibility paths.

**Show privacy:** Shows now save private audio for recovery and video export. Voice expires seven
days after the original recording, even when the Show is kept. Ordinary dictation stays in memory.

- **Review and recover Shows.** Browse saved Shows, navigate narration and numbered marks, inspect
  retained frames, and copy a moment's image with its summary and narration. Keep or discard
  bundles, recover interrupted recordings, or retry failed narration without overwriting earlier
  Shows or your edits.
- **Voiced, annotated video.** Windows Shows and Mac screen and area Shows retain video for playback
  and precise frame retrieval. Export a separate MP4 with timed ink, marks, original voice when
  available, and selectable captions when narration has word timings. Export requires local FFmpeg
  5.1 or newer with H.264 and AAC support and retained video. Exports are silent with a notice when
  voice is unavailable or expired. Mac Show capture remains experimental, and window Shows use still
  images.
- **Portable bundles.** Export ZIP bundles containing the edited summary, manifest, contact sheets,
  keyframes, and available source samples. Saved voice, recovery audio, and retention markers are
  excluded. Explicit ZIP and MP4 exports stay where you save them and do not expire automatically.
- **Everyday dictation.** Add phrase corrections, save named dictation presets, and choose local
  text styles including Verbatim, camelCase, and snake_case. Paste the last completed dictation
  again from memory. Mac users can choose Fn, Right Option, or Right Command. English filler removal
  now requires English to be selected; Automatic and other languages preserve words.
- **Local speech.** Optional Nemotron adds continuous recognition and recognition hints. Parakeet
  starts recognition during longer dictation to reduce work after release. Both deliver text once,
  when you finish. Quiet-speech handling improves; switching engines remains explicit.
- **Agent integrations.** Scoped, read-only MCP tools let an agent list and inspect Shows and
  retrieve images from folders you explicitly grant. Setup prints configuration for Claude Code,
  Codex, and other MCP clients. Send an edited summary and its contact sheets to an explicitly
  selected OpenCode session; a successful handoff confirms queue acceptance. These workflows can
  expose the requested content to the agent's model provider. They exclude audio and movies.
- **Scriptable controls.** Status, dictation start/stop/cancel, style, preset, and paste-last
  commands communicate with the running app. Owner acknowledgements and recording-specific tokens
  prevent stale commands from stopping a newer recording; uncertain outcomes are reported
  explicitly.
- **Settings and accessibility.** Dictation, Words, and Advanced pages organize setup, private
  practice, corrections, and model choices. Settings shows observed permission status. Windows adds
  accessible controls and text selection; complete screen-reader navigation remains unfinished.
- **Reliability and packages.** Improve cancellation, shutdown, capture geometry, checkpoint
  recovery, and export cleanup. Windows gains a per-user installer and authenticated update
  preparation; macOS gains a DMG alongside its ZIP.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows expire
after seven days unless kept; keeping a Show never extends its audio retention.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
