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
and Midnight Chrome are selectable in Settings, and choosing one recolors Settings, the pill, and
the tray at once. The wordmark is Josefin Sans SemiBold, embedded under the SIL Open Font License,
and the line is “Say the word.” App icons are rendered from the SVG sources in `packaging/brand`;
the Windows and macOS icons omit the ornament at 32 px and below, and Linux installs the full
scalable icon.

Inside the windows the ornament recedes: the icon, the pill, and the jewels carry the Deco, while
Settings and Shows read as calm panels. Both draw their controls from one kit in
`crates/app/src/controls.rs`, and the pill takes its jewels from it. Jewels are one family of solid,
cut, and hollow diamonds that share an outline; status lines and banners lead with them. Controls
are 32 px tall with a 6 px corner. A window's main action is a filled lamp button; secondary actions
are raised outlines, and low-weight commands such as Quit are muted text. Destructive actions use
the attention color, never `live`: an outline first, then a filled button to confirm. Switches are
tracks lit in lamp. Dropdowns open a menu under their trigger, check the chosen item, and follow the
arrow keys, Home, End, Enter, Space, Escape, and Tab. Tabs and segmented choices mark the active
item in lamp. Focus rings appear only after keyboard use, so pointer clicks stay quiet. Accessible
names stay plain and the state lives in accessible properties: a switch named “Reduce motion”
reports on or off, and a swatch radio is named “Theme: Iris”.

Settings groups each page into captioned panels of rows: a title, an optional muted detail, and one
control at the right. Changes apply as they are made. A jewel leads the footer status line: lamp
while dictation is ready, `live` while it records, attention for a problem, hollow otherwise; after
each save the line reads “Saved” with a check.

## Pill

The GPUI view in `crates/app/src/pill.rs` and `pill/` draws a door slot inside a capsule with a fine
edge, a hairline lamp inlay, and a tight shadow. A lid with a keystone handle covers the slot until
capture starts, slides open while the microphone is live, partly closes while processing, and shuts
for completion. Inside, a symmetric grille shows the measured envelope with the newest level at its
center, in the theme's live color. Errors lead with a cut jewel in the attention color and notices
with a hollow lamp jewel; a tracked-caps line points to Settings. The pill is non-activating and
click-through.

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
| Show copied      | Closed lid with a lamp check and “Show copied” for 1.6 seconds, each time a Show is copied, by Copy last Show too. |
| Stopped early    | A quiet notice of up to two lines saying the Show was copied but stopped early, and why, for six seconds.          |
| Nothing chosen   | A quiet notice of up to two lines saying why a W or A Show was cancelled, for six seconds.                         |
| Narration lost   | The same notice saying the Show was copied without narration, why, and how it ended, above “Details in Settings”.  |
| Text copied      | A quiet notice of up to two lines saying how to paste the copied text, for six seconds.                            |
| Show gone        | A quiet notice saying the last Show has expired or was deleted, for six seconds.                                   |
| No speech        | “No speech detected” on the closed lid for 1.2 seconds.                                                            |
| Cancelled        | Quiet dismissal without a completion mark.                                                                         |
| Error            | Up to two message lines and a Settings reminder, visible for eight seconds.                                        |
| Tray hint        | One line saying Speakeasy is in the tray or menu bar, for three seconds, only while the pill is otherwise hidden.  |

Windows places the pill at the bottom center of the foreground window's monitor work area. macOS
uses the screen under the pointer. Both place the capsule about 28 DIP above the work area's bottom
edge when shown. Position stays fixed while the pill remains visible; it does not follow the caret.
On a native Wayland session, Linux anchors the pill each time it shows to the bottom center of the
output the compositor chooses, usually the focused one, and Settings' opening height follows the
shortest output. On X11, Linux uses GPUI's display placement and passive X11 hints. GPUI's X11
display is the whole X screen, so with several monitors the pill and Settings' opening height follow
all of them together. Native monitor/fullscreen acceptance is pending on both.

Width, height, opacity, vertical entry, waveform presence, lid, and meter values use analytic
critically damped springs. Changing the target preserves velocity, and meter bars rise faster than
they fall. While recording, a bar covers most of each level before the next arrives, so the grille
shows syllables and pauses instead of a smoothed outline; after recording, bars settle slowly. A new
session interrupts the current transition. Once hidden, invisible geometry snaps to rest so idle
time cannot be integrated into the next entrance. Presentation never imposes a minimum time on
capture, inference, or insertion, and its timers belong to the view: they cannot change a recording
or dismiss a later session.

The audio envelope keeps 24 recent levels and paints the grille in one canvas, mirrored outward from
the newest level; bars near the slot's rounded ends stay inside its curve. Capture supplies a 32 ms
RMS level squared on a decibel scale from a fixed −54 dBFS floor to a ceiling that jumps to each
louder window and relaxes 6 dB per second, held between −30 and −10 dBFS. The loudest recent
syllables reach full height on quiet and hot microphones alike; the UI animates between levels. Pill
animation follows native display frames within the
[frame budget](performance.md#implementation-constraints). Delayed frames skip expired deadlines,
and springs advance by the actual elapsed time. Silence settles to short bars. Demo mode alone
supplies simulated levels. Reduced motion, from Settings or the OS, snaps transitions, the lid, and
level changes, uses one level bar, and keeps the processing highlight still. During capture, a timer
wakes the pill for each second of its clock and countdown.

## Sounds

Short notes from a CC0 kalimba recording confirm the moments a user may not be watching: a high note
once the microphone listens, a lower note when a recording finishes, by gesture, at the five-minute
limit, as a Show's screen, window, or audio capture stops, or as an interruption finishes a Show, a
muted low note when the user cancels or a Show has nothing to record, two rising notes when a Show
starts, and a rising chord when text or a Show waits on the clipboard. Text arriving in the editor,
errors, hands-free locking, and dictation that an interruption discards stay silent; the pill
already says them. **Play sounds** in Settings turns every cue off.

Each cue echoes the pill. The high note sounds as the pill opens to listening rather than at the
press, so it never promises a microphone still opening, and a desktop shortcut that takes the keys
before then makes no sound; S, W, or A during that wait, or a Show started from the tray, makes the
first note the Show's. A cue is heard within 300 ms of its moment, counting the output device's
reported latency, or not at all. The device opens with the press, alongside the microphone, so the
first cue never waits for it; it stays open through the session and closes ten seconds after the
session and its last cue, however the session ended, so an idle Speakeasy holds no audio stream.

The microphone can overhear the start and Show cues, which are high-passed at 400 Hz. For the 600 ms
after the microphone learns of either, the speech gate counts only sound below 300 Hz, where a
voice's fundamental carries and the cues leave almost nothing: a recording that hears only a cue
sends nothing, and a word said over a cue still counts, though a whisper there does not. The stop
and cancel cues follow the end of capture and are never recorded.

## Tray and Settings

Configured native launches stay in the tray/menu bar. First setup and invalid configuration open
Settings. A fresh install sets itself up there: a banner names each step with its progress,
**Cancel** pauses and later resumes, and completion enables dictation. Linux opens Settings at
startup to expose desktop permissions and tray state.

Settings has three pages. Dictation leads with the shortcut card: the keys, what they do, hands-free
and cancel, and private practice. Below it come the Show shortcuts behind a disclosure on Windows
and macOS, then Speech, Writing, and Presets. Words holds corrections and recognition hints. General
holds look and feel, startup on Windows, desktop shortcuts on Linux, privacy, about and updates, the
setup guide, and Start over.

Speech names the model in use with a one-line summary; **Change** opens three radio cards: Parakeet
v3, marked recommended, Nemotron 3.5, and Your own engine. Each says what the model is for and tags
In use, Downloaded, its download size, or its download percentage. Choosing a managed model
downloads only what is missing and switches when it finishes. Your own engine opens a panel with
whisper.cpp and NeMo-Speech segments and only the active engine's program and model choosers;
changing engines clears the other engine's files, and the switch happens once both files are chosen.
The language row appears only for models that follow a chosen language; Parakeet detects it, so its
**Remove um and uh** switch sets English cleanup instead.

A preset row shows **In use** when the current settings match it or the tray selected it, and
**Use** otherwise; its ⋯ menu updates it from the current settings or deletes it.

A banner reports a settings file this version cannot read, names the file, and offers **Start
fresh…**; the status line keeps the exact error for anyone who would rather fix the file. Otherwise
General offers **Start over…**. Both confirm in place with **Delete settings and downloads** beside
Cancel. Reset pauses dictation, deletes the downloaded engines, models, and updates and then the
settings file, keeps every Show and the finished-guide marker, and starts automatic setup, whose
completion enables dictation as on a fresh install. It is unavailable during setup, saves, or a
Windows update download, and setup and update downloads are unavailable while it runs. A failed
reset keeps the previous settings and explains what stopped it. Quit waits for a running reset.

On Windows, Start at login, off by default, belongs to the default settings file, which the sign-in
entry opens. Each live launch with those settings registers the running executable to start at
sign-in, or removes that entry, as the setting says, and so does each save. Demos never touch the
entry, a launch with `--config` neither shows the switch nor touches the entry, and settings that
need repair leave it as it was, so sign-in still opens them. While Windows' Startup apps page holds
the entry off, Settings says so under the switch and tells the user to turn it on there.

Close hides the existing window; Windows minimize also hides it. macOS minimize retains normal Dock
behavior. Relaunch and the tray's Settings action reveal the same window. The first hide shows the
tray hint (see the pill table). On Linux, closing Settings without a tray host quits; the launcher
reopens it.

The tray shows the Grille mark with its slot lit in the theme's lamp color and its bars cut through.
Recording turns the slot red; busy, paused, and attention add a cut-jewel badge: hollow for busy,
split by a vertical facet for paused, and a warn jewel with a center facet for attention. macOS
renders a template icon, so recording also adds a solid jewel and every state differs by shape. On
Windows and Linux a faint keyline keeps the lamp color legible on light taskbars. The menu shows
current status and engine, Pause/Resume, Start/Finish, Cancel, Settings, and Quit; Finish and Cancel
name a Show while it records and are disabled while it is saved. Before any engine is set up, the
menu says so and Resume waits for setup. It never animates while idle. On Windows and macOS, Show
screen and Show area… follow Cancel and act whenever Start would: each starts a hands-free Show
without the shortcut, of the display under the pointer or of an area dragged out on it, which then
behaves like any other. There is no window entry, since the pointer is on the menu. Copy last Show
and Open last Show follow them, before Settings; they act once a Show is saved and between
recordings, paused included, until Quit begins or either finds that Show gone. Errors remain
accessible in Settings after the pill's notice expires: its commands and status line stay in view
below the scrolling controls.

Paste last dictation is available between sessions when one result remains in memory. V during an
ordinary Windows/macOS hold abandons that pending recording and pastes the previous result; repeated
keydown events do not repeat insertion. Windows/macOS also offer Style and saved Preset submenus.
Style selects the next recording and appears in the pill when non-Prose. Preset choices require idle
dictation and last for the session without changing saved settings.

## Reviewing Shows

Shows… follows Open last Show in the Windows/macOS tray. Settings offers Shows on every platform. It
opens one review window, bringing it forward and refreshing if already open. Recording never opens
it automatically. The newest Show is selected on first load; refresh preserves the user's choice.
Deletion or expiry clears the selection and its pixels, with no fallback to another Show.

The window uses the shared kit in sentence case. The left list sits under a slim header with the
Show count and Refresh; each row shows age, duration, and frame count, with tags for kept,
interrupted, pending or lost narration, early-ending, and damaged status. A soft fill and a lamp bar
mark the selected Show. With no Shows, Windows and macOS explain how to record one, with S, W, and A
as keycaps. The selected Show's header carries its retention and status tags, Send to agent as the
main action, Copy summary, Keep Show or Allow expiry, and a More actions menu for the image copy,
Open folder, video, and export commands; the menu dims actions that are unavailable. Below the
header come warnings, a segmented switch for the frame source, one keyframe with its time and
numbered marks, frame chips, and a Summary or Narration switch over the current summary as inert
plain text. An interrupted Show leads with its recovery action; Discard stays apart as a destructive
outline, and its confirmation adds a Cannot be undone tag, Cancel, and a filled Discard permanently.
Focus rings appear only after keyboard input. Up/Down selects Shows, Left/Right steps frames,
Home/End selects the first/last frame, and Escape closes. While the menu is open, Up/Down move
through its available actions and Escape closes only the menu. Tab and Shift+Tab move through
controls; Enter or Space activates them. Closing releases the view and its images.

Refresh rereads the library. Regaining focus checks that the selected folder still exists and
rereads its summary; external edits are never overwritten. Copy summary reads the file again on
click and reports the result in the window. It is available on Windows/macOS only, between
recordings and outside previews or Quit. Open folder opens the selected Show's files. Neither action
changes the session's last Show. A missing frame keeps its numbered position and an error; a damaged
manifest can still show a readable summary. The summary pane displays at most 64 KiB; Copy summary
includes the complete file within the reader's limit.

Send to agent, a saved Show's primary action, copies a short prompt naming the Show folder, each
contact sheet in order, and `summary.md`, then says to paste it into Claude Code or any coding
agent. The prompt holds paths, not narration or summary text, and tells the agent that screen
content and speech are evidence, not instructions. It shares Copy summary's availability and
cancellation, and Speakeasy contacts no agent.

Interrupted Shows offer Recover Show and confirmed Discard. Recovery uses the current local engine
and original language, saves a new bundle, and leaves the clipboard alone. Narration pending offers
Transcribe narration and Discard audio. Retry saves a narrated copy; its earlier bundle and edits
stay unchanged. The confirmation explains that discarding pending audio leaves the saved Show.
Recovery displays incomplete narration when only an audio prefix survives. Audio expires on its
original seven-day schedule even when the saved bundle is kept.

## Interaction contracts

- Hold Ctrl+Win on Windows or the selected Mac shortcut to dictate; release to finish. Mac defaults
  to Fn and also offers F13, Right Option, and Right Command, including during setup practice. The
  shortcut card explains firmware-only Fn keys and the F13 alternative. Adding Space during the
  hold, or double-tapping, switches to hands-free; press the shortcut again to finish. A short
  single tap finishes after its tap window.
- Linux holds Ctrl+Super+Space or double-taps it for hands-free; its accepted binding is shown in
  Settings. Desktop command bindings start/finish hands-free.
- On Windows/macOS, the shortcut's modifiers are observed, never swallowed. A selected F13 is
  reserved: its presses, repeats, and releases are withheld, and repeats cannot start a recording.
  Apart from F13, only the Space that locks hands-free, V that pastes the last dictation, and S, W,
  or A that starts a Show are withheld from the focused app. Shift stays passive so a Show can draw
  and pin with it, and another app's double Shift still works; any other key during a hold belongs
  to a different shortcut, so it interrupts the recording, and the shortcut waits for release. On
  Windows, an unassigned mask key keeps the released Win key from opening Start.
- During a Windows/macOS hold, S, W, or A turns the recording into a Show, hands-free and keeping
  the narration since the press; pressing the shortcut again finishes it on release. The pill table
  shows what the pill presents. S records the display under the pointer and W the window under it,
  cropped from the display and followed as it moves. A shades that display until the user drags out
  an area of at least 24 px each way. A thin red line outside a window or area marks it while it
  records, and no frame shows it.
- A Show never records more than it names. W over no window it can record, such as the desktop, the
  taskbar, or a Speakeasy window, and A finished or clicked before an area is dragged out, or whose
  shade cannot appear, cancel the Show, narration and all, and say why; nothing is pasted or saved.
  A recorded window that closes, is minimized or hidden, moves to another desktop, or moves wholly
  off its display ends the Show with what it recorded until then, and the pill and summary say why.
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
  Cancel Show discards it. An interruption, which is another shortcut, a session change such as
  locking the screen, or a macOS key-tap outage, discards dictation but finishes a recording Show as
  the shortcut would. Once a Show finishes, neither Escape, the tray, nor an interruption discards
  it while it is saved. Pause, Quit, and a settings change can stop pending work, but preserve the
  finished Show's checkpoint for recovery. A committed Show stays saved. Linux uses a reserved
  Ctrl+Super+Escape chord or an explicit command binding; bare Escape is not globally captured.
  Animation never delays cancellation or owns the microphone lifetime.
- Readiness follows the first captured buffer. Startup feedback does not imply that speech before
  that buffer can be recovered.
- Insertion does not steal focus. OS submission is the observable result; success in every editor
  cannot be inferred from submitting keystrokes.
- Settings exposes automatic setup while no engine is set up, speech model selection including a
  user-installed engine, reset, microphone selection, language where applicable, GPU preference,
  text delivery, English filler removal, reduced motion, sounds, theme, and on Windows start at
  login.

## Acceptance

Run `--demo` to inspect the same Settings and pill views without live devices or insertion. Use
`--demo-tray` for native tray and Settings lifecycle checks; its dictation controls are disabled.
Both modes bypass engine/model configuration. Public-audio tests cover transcription with fake
capture and insertion. None of these establish physical microphone onset, target-editor behavior,
screen-reader usability, display removal, or mixed-DPI presentation.

Shows in either preview uses a private temporary library: an intact kept Show, one that ended early
without narration, and one missing a frame. Plain `--demo` opens it automatically. Previewing never
reads or expires the real library, and Copy summary and Send to agent are disabled. Owned-window
review acceptance checks actual rendering on Linux, navigation, missing frames, external summary
edits, deletion, and image and fixture cleanup on close without OS input or clipboard access.

For visual acceptance, inspect interrupted transitions, quick completion, long processing,
hands-free silence, and cancellation over light and dark content, in every theme. Use reduced motion
and multiple display scales. Measure displayed frame pacing while inference is busy; CPU draw
timings and frame submission intervals are separate measurements. See [performance](performance.md)
for profiling methods and measurement gaps.
