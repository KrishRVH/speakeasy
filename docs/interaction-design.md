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
| Starting | 56 × 28 | Neutral dot while capture opens. |
| Recording | 148 × 36 | Live dot and measured audio envelope. |
| Hands-free | 188 × 36 | Ringed live dot, envelope, and elapsed timer. |
| Stopping | Recording size | Live indication remains until capture stops. |
| Processing | 72 × 24 | Neutral line; moving highlight appears after 250 ms. |
| Error | 360 × 44 | Short actionable message, visible for eight seconds. |

Windows places the pill at the bottom center of the foreground window's monitor
work area. macOS uses the screen under the pointer. Both place the capsule about
28 DIP above the work area's bottom edge when shown. Position stays fixed while
the pill remains visible; it does not follow the caret.

Width, height, opacity, and meter values use analytic critically damped springs.
Changing the target preserves velocity. A new session continues from the current
visual shape. Completion and cancellation fade the pill without imposing a
minimum display time on inference or insertion.

The audio envelope has 24 columns painted in one canvas. Capture supplies RMS
summaries; the UI keeps recent levels and animates between them. Silence settles
to a thin line. Demo mode alone supplies simulated levels. Reduced motion snaps
the capsule's transitions, uses one smoothed level bar, and keeps the processing
indicator static. App animation stops requesting frames once it settles, except
for visible processing motion and the hands-free timer.

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
insertion. The Windows demo has been checked for visibility, focus preservation,
and click-through. Public-audio tests cover recognition with fake capture and
insertion. These checks do not establish physical microphone onset, target-editor
behavior, screen-reader usability, display removal, or mixed-DPI presentation.

For visual acceptance, inspect interrupted transitions, quick completion, long
processing, hands-free silence, and cancellation over light and dark content.
Use reduced motion and multiple display scales. Measure displayed frame pacing
while inference is busy; CPU draw timings and frame submission intervals are
separate measurements. See [performance-audit.md](performance-audit.md) for the
available evidence and gaps.
