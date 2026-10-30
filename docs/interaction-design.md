# Interaction design

Speakeasy centers on one task: dictate into the application where the user is already working. The
name supplies the organizing idea: a speakeasy is a room behind an ordinary door, entered by saying
the word at a small slot, where what is said stays inside. The tray icon is the door, the pill is
the slot, and the house rule is local-only dictation. Feedback should be calm, continuous, and
truthful about capture and processing.

## Brand

The visual language is Art Deco with restraint: stepped forms, keystone diamonds, and fine inlaid
rules. Three rules keep it fast and legible. Ornament scales with size: the app icon carries the
full inlay, the pill a hairline, and the 16 px tray glyph only its silhouette. Ornament holds still;
only voice level, processing, and submission move. Red means on air: each theme's `live` color
appears only while capture is open, and its `lamp` accent never signals recording.

`crates/app/src/theme.rs` defines four themes. Jet & Champagne is the default; Emerald Lounge, Iris,
and Midnight Chrome are selectable in Settings. Settings previews an unsaved theme; the pill and
tray change when it is saved. The Settings wordmark is Josefin Sans SemiBold, embedded under the SIL
Open Font License, and the line is “Say the word.” App icons are rendered from the SVG sources in
`packaging/brand`; the Windows and macOS icons omit the ornament at 32 px and below, and Linux
installs the full scalable icon.

## Pill

The GPUI view in `crates/app/src/pill.rs` and `pill/` draws a door slot inside a capsule with a fine
edge, a hairline lamp inlay, and a tight shadow. A lid with a keystone handle covers the slot until
capture starts, slides open while the microphone is live, partly closes while processing, and shuts
for completion. Inside, a symmetric grille shows the measured envelope with the newest level at its
center, in the theme's live color. Errors use the attention color and text. The pill is
non-activating and click-through.

| State            | Content                                                                                                            |
| ---------------- | ------------------------------------------------------------------------------------------------------------------ |
| Starting         | Closed lid with a microphone symbol and “Opening mic…”.                                                            |
| Recording        | Open slot, live rim, and measured grille.                                                                          |
| Hands-free       | Grille, lock symbol, and elapsed timer beside the slot.                                                            |
| Show recording   | Hands-free layout with a red screen mark for the lock; “Hold Shift to draw” replaces the timer for three seconds.  |
| Final 30 seconds | Remaining time in the attention color; the recording limit stays in the session owner.                             |
| Stopping         | The lid stays open until capture stops; the grille settles.                                                        |
| Processing       | The lid partly closes; a lamp highlight sweeps the opening after 250 ms.                                           |
| Submitted        | Closed lid with a lamp check for 350 ms, then collapse and fade.                                                   |
| Show copied      | Closed lid with a lamp check and “Show copied” for 1.6 seconds, once the bundle is written and its summary copied. |
| Screen stopped   | A muted notice of up to two lines saying the Show was copied but its screen stopped early, for six seconds.        |
| Text copied      | A muted notice of up to two lines saying how to paste the copied text, for six seconds.                            |
| No speech        | “No speech detected” on the closed lid for 1.2 seconds.                                                            |
| Cancelled        | Quiet dismissal without a completion mark.                                                                         |
| Error            | Up to two message lines and a Settings reminder, visible for eight seconds.                                        |
| Tray hint        | One line saying Speakeasy is in the tray or menu bar, for three seconds, only while the pill is otherwise hidden.  |

Windows places the pill at the bottom center of the foreground window's monitor work area. macOS
uses the screen under the pointer. Both place the capsule about 28 DIP above the work area's bottom
edge when shown. Position stays fixed while the pill remains visible; it does not follow the caret.
Linux uses GPUI's display placement and passive X11 hints. GPUI's X11 display is the whole X screen,
so with several monitors the pill and Settings' opening height follow all of them together; native
monitor/fullscreen acceptance is pending.

Width, height, opacity, vertical entry, waveform presence, lid, and meter values use analytic
critically damped springs. Changing the target preserves velocity, and meter bars rise faster than
they fall. A new session interrupts the current transition. Once hidden, invisible geometry snaps to
rest so idle time cannot be integrated into the next entrance. Presentation never imposes a minimum
time on capture, inference, or insertion, and its timers belong to the view: they cannot change a
recording or dismiss a later session.

The audio envelope keeps 24 recent levels and paints the grille in one canvas, mirrored outward from
the newest level; bars near the slot's rounded ends stay inside its curve. Capture supplies RMS
summaries; the UI animates between them. Pill animation follows native display frames within the
[frame budget](performance.md#implementation-constraints). Delayed frames skip expired deadlines,
and springs advance by the actual elapsed time. Silence settles to short bars. Demo mode alone
supplies simulated levels. Reduced motion, from Settings or the OS, snaps transitions, the lid, and
level changes, uses one level bar, and keeps the processing highlight still. During capture, a timer
wakes the pill for each second of its clock and countdown.

## Sounds

Short notes from a CC0 kalimba recording confirm the moments a user may not be watching: a high note
once the microphone listens, a lower note when a recording finishes, by gesture or at the
five-minute limit, a muted low note when the user cancels, two rising notes when a Show starts, and
a rising chord when text or a Show waits on the clipboard. Text arriving in the editor, errors,
hands-free locking, and a hold that another shortcut, a session change, or a macOS key-tap outage
ends stay silent; the pill already says them. **Play sounds** in Settings turns every cue off.

Each cue echoes the pill. The high note sounds as the pill opens to listening rather than at the
press, so it never promises a microphone still opening, and a desktop shortcut that takes the keys
before then makes no sound; S, W, or A during that wait makes the first note the Show's. A cue is
heard within 300 ms of its moment, counting the output device's reported latency, or not at all. The
device opens with the press, alongside the microphone, so the first cue never waits for it; it stays
open through the session and closes ten seconds after the session and its last cue, however the
session ended, so an idle Speakeasy holds no audio stream.

The microphone can overhear the start and Show cues, which are high-passed at 400 Hz. For the 600 ms
after the microphone learns of either, the speech gate counts only sound below 300 Hz, where a
voice's fundamental carries and the cues leave almost nothing: a recording that hears only a cue
sends nothing, and a word said over a cue still counts, though a whisper there does not. The stop
and cancel cues follow the end of capture and are never recorded.

## Tray and Settings

Configured native launches stay in the tray/menu bar. First setup and invalid configuration open
Settings. A fresh install sets itself up there: a card names each step with its progress, **Cancel**
pauses and later resumes, and completion enables dictation. The manual engine controls stay hidden
while setup runs. Linux opens Settings at startup to expose desktop permissions and tray state.

Close hides the existing window, preserving edits; Windows minimize also hides it. macOS minimize
retains normal Dock behavior. Relaunch and the tray's Settings action reveal the same window. The
first hide shows the tray hint (see the pill table). On Linux, closing Settings without a tray host
quits; the launcher reopens it.

The tray shows the Grille mark with its slot lit in the theme's lamp color and its bars cut through.
Recording turns the slot red; busy, paused, and attention add a cut-jewel badge. macOS renders a
template icon, so recording also adds a diamond badge and every state differs by shape. On Windows
and Linux a faint keyline keeps the lamp color legible on light taskbars. The menu shows current
status and engine, Pause/Resume, Start/Finish, Cancel, Settings, and Quit; Finish and Cancel name a
Show while it records and are disabled while it is saved. Before any engine is set up, the menu says
so and Resume waits for setup. It never animates while idle. Errors remain accessible in Settings
after the pill's notice expires, including while there are unsaved edits: its commands and status
line stay in view below the scrolling controls.

## Interaction contracts

- Hold Ctrl+Win on Windows or Fn on macOS to dictate; release to finish. Adding Space during the
  hold, or double-tapping, switches to hands-free; press the shortcut again to finish. A short
  single tap finishes after its tap window.
- Linux holds Ctrl+Super+Space or double-taps it for hands-free; its accepted binding is shown in
  Settings. Desktop command bindings start/finish hands-free.
- On Windows/macOS, the shortcut's modifiers are observed, never swallowed. Only the Space that
  locks hands-free and the S, W, or A that starts a Show are withheld from the focused app. Shift
  stays passive so a Show can draw and pin with it, and another app's double Shift still works; any
  other key during a hold belongs to a different shortcut: dictation cancels and waits for release.
  On Windows, an unassigned mask key keeps the released Win key from opening Start.
- During a Windows/macOS hold, S, W, or A turns the recording into a Show, hands-free and keeping
  the narration since the press; pressing the shortcut again finishes it on release. The pill table
  shows what the pill presents. S records the display under the pointer and W the window under it,
  cropped from the display and followed as it moves; over the desktop, the taskbar, or a Speakeasy
  window, W records the display. A shades that display until the user drags out an area of at least
  24 px each way; finishing first, or a shade that cannot appear, records the whole display. A thin
  red line outside a window or area marks it while it records, and no frame shows it.
- Shift draws ink, alone or while the shortcut is still held; the ink holds briefly after the pen
  lifts, then fades, and clicks pass through to the apps underneath. Two taps of Shift alone, each
  released within 300 ms and the second pressed within 400 ms of the first's release, drop a pin at
  the pointer, unless either tap drew ink or clicked or another modifier was held; no pin lands off
  the recorded window or area. Marks and pins share one numbering in the order they began, ① ② ③;
  each number sits beside what it numbers on the overlay, fading with the ink, and in the bundle's
  keyframes and summary.
- Windows/macOS Escape continues to the focused app. It cancels dictation immediately at any stage,
  but a Show, area choice included, only while the shortcut is held: during the hold that started it
  or the press that would finish it. Otherwise a recording Show ignores Escape, and the tray's
  Cancel Show discards it. Once a Show finishes, neither Escape, the tray, nor an interruption
  discards it while it is saved; Pause, Quit, and a settings change that ends a recording still do.
  Linux uses a reserved Ctrl+Super+Escape chord or an explicit command binding; bare Escape is not
  globally captured. Animation never delays cancellation or owns the microphone lifetime.
- Readiness follows the first captured buffer. Startup feedback does not imply that speech before
  that buffer can be recovered.
- Insertion does not steal focus. OS submission is the observable result; success in every editor
  cannot be inferred from submitting keystrokes.
- Settings exposes automatic setup while no engine is set up, and manual engine/model selection,
  microphone selection, language where applicable, GPU preference, text delivery, English filler
  removal, reduced motion, sounds, and theme.

## Acceptance

Run `--demo` to inspect the same Settings and pill views without live devices or insertion. Use
`--demo-tray` for native tray and Settings lifecycle checks; its dictation controls are disabled.
Both modes bypass engine/model configuration. Public-audio tests cover transcription with fake
capture and insertion. None of these establish physical microphone onset, target-editor behavior,
screen-reader usability, display removal, or mixed-DPI presentation.

For visual acceptance, inspect interrupted transitions, quick completion, long processing,
hands-free silence, and cancellation over light and dark content, in every theme. Use reduced motion
and multiple display scales. Measure displayed frame pacing while inference is busy; CPU draw
timings and frame submission intervals are separate measurements. See [performance](performance.md)
for profiling methods and measurement gaps.
