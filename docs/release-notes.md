# Release notes

Speakeasy 0.10.1 adds F13 dictation for external Mac keyboards and fixes races in reset and Show
review.

- **External Mac keyboards.** Choose F13 in Settings or the setup guide, then use the keyboard's F13
  key or map a spare key to F13 in its keyboard software. This addresses keyboards such as the
  Keychron Q6 Ultra 8K whose Fn key switches firmware layers without sending a key to macOS.
  Speakeasy cannot observe that firmware-only Fn key. F13 supports hold-to-talk, double-tap
  hands-free, and Show gestures; repeats do not restart dictation or interrupt Shift pins. Text
  delivery waits for the selected shortcut to be released. Fn, Right Option, and Right Command
  remain available.
- **Stable microphone choices.** An open microphone menu keeps each row attached to its device when
  devices refresh, so a click cannot select a different microphone.
- **Reliable Show review.** Delayed clicks in a refreshed Show list cannot open a different Show.
  Retained source frames with a mismatched recording timeline are unavailable; saved keyframes and
  edited summaries remain readable. The agent reading guide accurately describes the edited summary
  and recorded narration included by the copy action.
- **Reset and updates.** Reset holds both setup and update locks until deletion finishes. The update
  lock lives outside the removable cache, preventing another copy from starting an update while
  reset removes its files.
- **Cleanup evaluation.** The public-fixture accuracy harness can compare recognition with cleanup
  enabled and disabled, measuring errors introduced and corrected by cleanup separately.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows expire
after seven days unless kept; keeping a Show never extends its audio retention.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
