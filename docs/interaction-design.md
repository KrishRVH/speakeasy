# Interaction design: Tally

Speakeasy centers on one task: dictate into the application where the user is
already working. The Tally direction, developed with Fable 5.1 at xhigh, uses a
broadcast recording light as its organizing idea. Feedback should be calm,
continuous, and truthful about capture and processing.

## Current pill

The GPUI view in `crates/app/src/pill.rs` uses a graphite capsule (`#17181B`), a
fine translucent edge, and a tight shadow. Red-orange (`#FF4F2E`) indicates
recording or capture stopping; neutral gray indicates startup and processing.
Errors use amber and text. The pill is non-activating and click-through.

| State | Size in DIP | Content |
| --- | --- | --- |
| Starting | 158 × 32 | Neutral microphone symbol and “Opening mic…”. |
| Recording | 148 × 36 | Live dot and measured audio envelope. |
| Hands-free | 212 × 36 | Live dot, envelope, lock symbol, and elapsed timer. |
| Final 30 seconds | 248 × 36 | Amber remaining time; the recording limit stays in the session owner. |
| Stopping | Recording size | Live indication remains until capture stops; envelope settles. |
| Processing | 88 × 28 | Envelope fades into a neutral line; highlight moves after 250 ms. |
| Submitted | 64 × 28 | Neutral check for 350 ms, then collapse and fade. |
| No speech | 166 × 32 | “No speech detected” for 1.2 seconds. |
| Cancelled | Collapsing | Quiet dismissal without a completion mark. |
| Error | 360 × 64 | Up to two message lines and a Settings reminder, visible for eight seconds. |

Windows places the pill at the bottom center of the foreground window's monitor
work area. macOS uses the screen under the pointer. Both place the capsule about
28 DIP above the work area's bottom edge when shown. Position stays fixed while
the pill remains visible; it does not follow the caret.

Width, height, opacity, vertical entry, and meter values use analytic critically
damped springs. Changing the target preserves velocity. Geometry uses a response
of 28/s, opacity 40/s, entry 32/s, and audio attack/release 48/s and 20/s. A new
session interrupts the current transition. Once hidden, invisible geometry snaps
to rest so idle time cannot be integrated into the next entrance. Presentation
never imposes a minimum time on capture, inference, or insertion.

The audio envelope has 24 columns painted in one canvas. Capture supplies RMS
summaries; the UI keeps recent levels and animates between them. Silence settles
to a thin line. Demo mode alone supplies simulated levels. Reduced motion snaps
transitions and level changes, uses one level bar, and keeps processing static.
Settled hidden views request no animation frames; capture uses a timer for its
clock and recording-limit cue.

## Tray and Settings

Configured native launches stay in the tray/menu bar. First setup and invalid
configuration open Settings. Close hides the existing window, preserving edits;
Windows minimize also hides it. macOS minimize retains normal Dock behavior.
Relaunch and the tray's Settings action reveal the same window. A brief first-hide
hint explains the tray/menu bar when the pill is idle.

The waveform tray mark uses distinct static badges for recording, busy, paused,
and attention, including shape differences for macOS template icons. The menu
shows current status and engine, Pause/Resume, Start/Finish, Cancel, Settings, and
Quit. It never animates while idle. Errors remain accessible in Settings after
the pill's notice expires, including while there are unsaved edits.

## Interaction contracts

- Hold Ctrl+Alt+Space to dictate; release to finish. Double-tap for hands-free,
  then press again to finish. A short single tap finishes after its tap window.
- Escape cancels immediately and continues to the focused app. Animation never
  delays cancellation or owns the microphone lifetime.
- Readiness follows the first captured buffer. Startup feedback does not imply
  that speech before that buffer can be recovered.
- Insertion does not steal focus. OS submission is the observable result;
  success in every editor cannot be inferred from submitting keystrokes.
- Settings exposes local engine/model selection, microphone selection, language
  and GPU preferences where applicable, clipboard preservation, and reduced
  motion. Errors remain visible there after the pill notice ends.

## Native acceptance

Run `--demo` to inspect the same Settings and pill views without live devices or
insertion. Use `--demo-tray` for native tray and Settings lifecycle checks; its
dictation controls are disabled. Both modes bypass engine/model configuration.
The Windows demo has been checked for visibility, focus preservation,
and click-through. Public-audio tests cover recognition with fake capture and
insertion. These checks do not establish physical microphone onset, target-editor
behavior, screen-reader usability, display removal, or mixed-DPI presentation.

For visual acceptance, inspect interrupted transitions, quick completion, long
processing, hands-free silence, and cancellation over light and dark content.
Use reduced motion and multiple display scales. Measure displayed frame pacing
while inference is busy; CPU draw timings and frame submission intervals are
separate measurements. See [performance-audit.md](performance-audit.md) for the
available evidence and gaps.
