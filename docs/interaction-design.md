# Interaction design

Speakeasy centers on one task: dictate into the application where the user is
already working. The name supplies the organizing idea: a speakeasy is a room
behind an ordinary door, entered by saying the word at a small slot, where what
is said stays inside. The tray icon is the door, the pill is the slot, and the
house rule is local-only dictation. Feedback should be calm, continuous, and
truthful about capture and processing.

## Brand

The visual language is Art Deco with restraint: stepped forms, keystone
diamonds, and fine inlaid rules. Three rules keep it fast and legible.
Ornament scales with size: the app icon carries the full inlay, the pill a
hairline, and the 16 px tray glyph only its silhouette. Ornament holds still;
only voice level, processing, and submission move. Red means on air: each
theme's `live` color appears only while capture is open, and its `lamp`
accent never signals recording.

`crates/app/src/theme.rs` defines four themes. Jet & Champagne is the default;
Emerald Lounge, Iris, and Midnight Chrome are selectable in Settings. Settings
previews an unsaved theme; the pill and tray change when it is saved. The
Settings wordmark is Josefin Sans SemiBold, embedded under the SIL Open Font
License, and the line is “Say the word.” App icons are rendered from the SVG
sources in `packaging/brand`; sizes of 32 px and below omit the ornament.

## Pill

The GPUI view in `crates/app/src/pill.rs` draws a door slot inside a capsule
with a fine edge, a hairline lamp inlay, and a tight shadow. A lid with a
keystone handle covers the slot until capture starts, slides open while the
microphone is live, closes to 62% while processing, and shuts for completion.
Inside, a symmetric grille shows the measured envelope with the newest level
at its center, in the theme's live color. Errors use the attention color and
text. The pill is non-activating and click-through.

| State | Size in DIP | Content |
| --- | --- | --- |
| Starting | 172 × 38 | Closed lid with a microphone symbol and “Opening mic…”. |
| Recording | 152 × 38 | Open slot, live rim, and measured grille. |
| Hands-free | 228 × 38 | Grille, lock symbol, and elapsed timer beside the slot. |
| Final 30 seconds | 262 × 38 | Remaining time in the attention color; the recording limit stays in the session owner. |
| Stopping | Recording size | The lid stays open until capture stops; the grille settles. |
| Processing | 104 × 32 | The lid half closes; a lamp highlight sweeps the opening after 250 ms. |
| Submitted | 76 × 34 | Closed lid with a lamp check for 350 ms, then collapse and fade. |
| No speech | 182 × 36 | “No speech detected” on the closed lid for 1.2 seconds. |
| Cancelled | Collapsing | Quiet dismissal without a completion mark. |
| Error | 360 × 64 | Up to two message lines and a Settings reminder, visible for eight seconds. |

Windows places the pill at the bottom center of the foreground window's monitor
work area. macOS uses the screen under the pointer. Both place the capsule about
28 DIP above the work area's bottom edge when shown. Position stays fixed while
the pill remains visible; it does not follow the caret. Linux uses GPUI's display
placement and passive X11 hints; native monitor/fullscreen acceptance is pending.

Width, height, opacity, vertical entry, lid, and meter values use analytic
critically damped springs. Changing the target preserves velocity. Geometry uses
a response of 28/s, opacity 40/s, entry 32/s, lid 24/s, and audio attack/release
48/s and 20/s. A new session interrupts the current transition. Once hidden,
invisible geometry snaps to rest so idle time cannot be integrated into the next
entrance. Presentation never imposes a minimum time on capture, inference, or
insertion, and its timers belong to the view: they cannot change a recording or
dismiss a later session.

The audio envelope keeps 24 recent levels and paints the grille in one canvas,
mirrored outward from the newest level; bars near the slot's rounded ends stay
inside its curve. Capture supplies RMS summaries; the UI animates between them.
Pill animation follows native display frames with a 200 FPS budget. Meter
updates share the pending frame; session changes redraw immediately. Delayed
frames skip expired deadlines, and springs advance by the actual elapsed time.
Silence settles to short bars. Demo mode alone supplies simulated levels.
Reduced motion snaps transitions, the lid, and level changes, uses one level bar,
and keeps the processing highlight still.
Settled hidden views request no animation frames; capture uses a timer for its
clock and recording-limit cue.

## Tray and Settings

Configured native launches stay in the tray/menu bar. First setup and invalid
configuration open Settings. A fresh install sets itself up there: a card names
each step with its progress, **Cancel** pauses and later resumes, and completion
enables dictation. The manual engine controls stay hidden while setup runs.
Linux opens Settings at startup to expose desktop permissions and tray state.

Close hides the existing window, preserving edits; Windows minimize also hides
it. macOS minimize retains normal Dock behavior. Relaunch and the tray's
Settings action reveal the same window. A brief first-hide hint explains the
tray/menu bar when the pill is idle.
On Linux, closing Settings without a tray host quits; the launcher reopens it.

The tray shows the Grille mark with its slot lit in the theme's lamp color and
its bars cut through. Recording turns the slot red; busy, paused, and attention
add a cut-jewel badge. macOS renders a template icon, so recording also adds a
diamond badge and every state differs by shape. On Windows a faint keyline keeps
the lamp color legible on light taskbars. The menu
shows current status and engine, Pause/Resume, Start/Finish, Cancel, Settings, and
Quit. It never animates while idle. Errors remain accessible in Settings after
the pill's notice expires, including while there are unsaved edits.

## Interaction contracts

- Hold Ctrl+Win on Windows or Fn on macOS to dictate; release to finish. Adding
  Space during the hold, or double-tapping, switches to hands-free; press the
  shortcut again to finish. A short single tap finishes after its tap window.
- Linux holds Ctrl+Super+Space or double-taps it for hands-free; its accepted
  binding is shown in Settings. Desktop command bindings start/finish hands-free.
- On Windows/macOS, the shortcut's modifiers are observed, never swallowed. Only the Space that
  locks hands-free is withheld from the focused app. Any other key during a hold
  belongs to a different shortcut: dictation cancels and waits for release. On
  Windows, an unassigned mask key keeps the released Win key from opening Start.
- Windows/macOS Escape cancels immediately and continues to the focused app.
  Linux uses a reserved Ctrl+Super+Escape chord or an explicit command binding;
  bare Escape is not globally captured. Animation never delays cancellation or
  owns the microphone lifetime.
- Readiness follows the first captured buffer. Startup feedback does not imply
  that speech before that buffer can be recovered.
- Insertion does not steal focus. OS submission is the observable result;
  success in every editor cannot be inferred from submitting keystrokes.
- Settings exposes automatic setup, manual engine/model selection, microphone
  selection, language where applicable, GPU preference, clipboard preservation,
  reduced motion, and theme. Errors remain visible there after the pill notice
  ends.

## Acceptance

Run `--demo` to inspect the same Settings and pill views without live devices or
insertion. Use `--demo-tray` for native tray and Settings lifecycle checks; its
dictation controls are disabled. Both modes bypass engine/model configuration.
Public-audio tests cover recognition with fake capture and insertion. None of
these establish physical microphone onset, target-editor behavior, screen-reader
usability, display removal, or mixed-DPI presentation.

For visual acceptance, inspect interrupted transitions, quick completion, long
processing, hands-free silence, and cancellation over light and dark content,
in every theme. Use reduced motion and multiple display scales. Measure
displayed frame pacing while inference is busy; CPU draw timings and frame
submission intervals are separate measurements. See
[performance](performance.md) for measured costs and gaps.
