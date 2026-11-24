# Interaction design

## Brand

The visual language is Art Deco with restraint: stepped forms, keystone diamonds, and fine inlaid
rules. Three rules keep it fast and legible. Ornament scales with size: the app icon carries the
full inlay, the pill a hairline, and the 16 px tray glyph only its silhouette. Ornament holds still;
only voice level, processing, and submission move. Red means on air: each theme's `live` color
appears only while capture is open, and its `lamp` accent never signals recording.

`crates/app/src/theme.rs` defines four themes. Jet & Champagne is the default; Emerald Lounge, Iris,
and Midnight Chrome are selectable in Settings, and choosing one recolors Settings, the pill, Shows,
and the Windows and Linux tray at once. The wordmark is Josefin Sans SemiBold, embedded under the
SIL Open Font License, and the line is “Say the word.” App icons come from the SVG sources in
`packaging/brand`: the committed Windows and macOS icon files omit the ornament at 32 px and below,
and Linux installs the full scalable icon.

Inside the windows the ornament recedes: the icon, the pill, and the jewels carry the Deco, while
Settings and Shows read as calm panels. Both draw their controls from one kit in
`crates/app/src/controls.rs`, and the pill takes its jewels from it. Jewels are one family of solid,
cut, and hollow diamonds that share an outline; status lines and banners lead with them. Controls
are 32 px tall with a 6 px corner. A window's main action is a filled lamp button; secondary actions
are raised outlines, and low-weight commands such as Quit are muted text. Destructive actions use
the attention color, never `live`: an outline first, then a filled button to confirm. Switches are
tracks lit in lamp. Dropdowns open a menu under their trigger, check the chosen item, and follow the
arrow keys, Home, End, Enter, Space, Escape, and Tab. Tabs mark the active item with a lamp rule;
segmented choices raise it. Focus rings appear only after keyboard use, so pointer clicks stay
quiet. Accessible names stay plain and the state lives in accessible properties: a switch named
“Reduce motion” reports on or off, and a swatch radio is named “Theme: Iris”.

Settings groups each page into captioned panels of rows: a title, an optional muted detail, and one
control at the right. Changes apply as they are made. A jewel leads the footer status line: lamp
while dictation is ready, `live` while it records, attention for a problem, hollow lamp beside
“Setting up…” while first setup installs a speech model, hollow otherwise; after each save the line
reads “Saved” with a check. Inside Settings the line names the problem; the tray adds “Open
Settings” to it.

## Pill

The GPUI view in `crates/app/src/pill.rs` and `pill/` draws a door slot inside a capsule with a fine
edge, a hairline lamp inlay, and a tight shadow. A lid with a keystone handle covers the slot until
capture starts, slides open while the microphone is live, partly closes while processing, and shuts
for completion. Inside, a symmetric grille shows the measured envelope with the newest level at its
center, in the theme's live color. Errors lead with a cut jewel in the attention color and notices
with a hollow lamp jewel; a tracked-caps line points to Settings. The pill is non-activating and
click-through.

| State            | Content                                                                                                                                                                                            |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Starting         | Closed lid with a microphone symbol and “Opening mic…”.                                                                                                                                            |
| Recording        | Open slot, live rim, and measured grille.                                                                                                                                                          |
| Hands-free       | Grille, lock symbol, and elapsed timer beside the slot.                                                                                                                                            |
| Show recording   | Hands-free layout with a red screen mark for the lock; “Hold Shift to draw” replaces the timer for three seconds.                                                                                  |
| Fn also acts     | A line under the slot naming what macOS also does on Fn and pointing to Settings, for five seconds, as the recording opens; for macOS Dictation, which a double press starts, as hands-free locks. |
| Secure input     | A line under the slot naming the app that turned it on and that Space, Return, and Esc won’t arrive, for five seconds.                                                                             |
| Default mic      | A line under the slot saying the chosen microphone isn’t connected and the default records, for four seconds.                                                                                      |
| Final 30 seconds | Remaining time in the attention color; the recording limit stays in the session owner.                                                                                                             |
| Stopping         | The lid stays open until capture stops; the grille settles.                                                                                                                                        |
| Processing       | The lid partly closes; a lamp highlight sweeps the opening after 250 ms.                                                                                                                           |
| Submitted        | Closed lid with a lamp check for 350 ms, then collapse and fade.                                                                                                                                   |
| Show copied      | Closed lid with a lamp check and “Show copied” for 1.6 seconds, each time a Show is copied, by Copy last Show too.                                                                                 |
| Stopped early    | A quiet notice of up to two lines saying the Show was copied but stopped early, and why, for six seconds.                                                                                          |
| Nothing chosen   | A quiet notice of up to two lines saying why a W or A Show was cancelled, for six seconds.                                                                                                         |
| Narration lost   | The same notice saying what narration the copied Show lacks, why, and how it ended, above “Details in Settings”.                                                                                   |
| Text copied      | A quiet notice of up to two lines saying how to paste the copied text, for six seconds.                                                                                                            |
| Microphone lost  | A quiet notice naming what stopped the microphone and how to paste the words it kept, for six seconds.                                                                                             |
| Show gone        | A quiet notice saying the last Show has expired or was deleted, for six seconds.                                                                                                                   |
| No speech        | “No speech detected” on the closed lid for 1.2 seconds.                                                                                                                                            |
| Cancelled        | Quiet dismissal without a completion mark.                                                                                                                                                         |
| Error            | Up to two message lines and a Settings reminder, visible for eight seconds.                                                                                                                        |
| Tray hint        | One line saying Speakeasy is in the tray or menu bar, for three seconds, only while the pill is otherwise hidden.                                                                                  |

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

The grille mirrors recent levels from the center. Its scale adapts to the loudest recent speech, so
quiet and hot microphones both have usable range. Reduced motion snaps transitions, substitutes one
bar, and holds the processing highlight still. A recording timer still updates once a second.

## Sounds

Short notes from a CC0 kalimba recording confirm the moments a user may not be watching: a high note
once the microphone listens, a lower note when a recording finishes, by gesture, at the five-minute
limit, as its microphone is lost after speech, as a Show's screen, window, or audio capture stops,
or as an interruption finishes a Show, a muted low note when the user cancels or a Show has nothing
to record, two rising notes when a Show starts, and a rising chord when text or a Show waits on the
clipboard, by **Copy last Show** too while dictation runs. Text arriving in the editor, errors,
hands-free locking, and dictation that an interruption discards stay silent; the pill already says
them. **Play sounds** in Settings turns every cue off.

The output device opens alongside the microphone and stays through the session. If the cue would
arrive more than 300 ms after its moment, it is skipped. The speech gate favors the voice band while
it can hear an opening or Show cue, so hearing only the cue does not qualify as speech.

## Tray and Settings

Settings has three pages with a shared footer for status and app actions. Dictation leads with the
shortcut and private practice, followed by speech, writing, and presets. Words keeps corrections and
hints together. General holds app-wide choices, platform settings, and actions such as reset.
Settings reports the actual error and a remedy after the pill fades. Preset rows keep their identity
as their Use, Update, or Delete actions change.

Windows and Mac use the same tray menu policy. The Mac icon uses its badge rather than color to show
recording. Settings close hides its existing window and leaves saves and setup running. Relaunch
reveals that window. On Linux without a tray host, closing Settings quits and the launcher reopens
it.

## Reviewing Shows

The library labels kept, interrupted, pending narration, early endings, and unreadable Shows.
Refresh keeps the selected Show when present. Deletion or expiry clears selection and pixels. Read
and export errors belong to the Show that produced them. Use the shared controls for navigation and
for Keep, Export, and Copy. Discard and Delete use a confirmation tied to the selection, invalidated
by refresh or a new selection.

Summary is the user's editable file, and Narration is the recorded speech. Switching between them
keeps summary edits. Timed marks and clauses carry a requested time, and the caption names the
actual displayed sample. Switching frame modes keeps the request. Manual navigation or a new
selection clears it. Use the [Show guide](../README.md#read-saved-shows) for review actions.

## Interaction contracts

- Hold the selected Windows or Mac shortcut to dictate; release to finish. Windows defaults to
  Ctrl+Win and also offers Right Ctrl and F13; Mac defaults to Fn and also offers F13, Right Option,
  and Right Command, both including during setup practice. The shortcut card explains firmware-only
  Fn keys, mapping a spare key to F13, and the quick-combination rule. With Fn selected, the card
  warns, and the pill notes as each recording opens, when macOS also acts on Fn presses (“Press 🌐
  key to”), which can open a picker that catches the paste; a Globe key event Fn sends beside its
  flag change is part of the Fn press, never another key. With a right modifier, a hold key pressed
  within 250 ms of it interrupts and reaches the app, so Right Ctrl+C still copies; pressed when
  another modifier is already down, the right modifier does not dictate. Adding Space during the
  hold, or double-tapping, switches to hands-free; press the shortcut again to finish. A short
  single tap finishes after its tap window.
- Linux holds Ctrl+Super+Space or double-taps it for hands-free; its accepted binding is shown in
  Settings. Desktop command bindings start/finish hands-free.
- On Windows/macOS, the shortcut's modifiers are observed, never swallowed. A selected F13 is
  reserved: its presses, repeats, and releases are withheld, and repeats cannot start a recording.
  Apart from F13, only the Space that locks hands-free, Return that sends, V that pastes the last
  dictation, and S, W, or A that starts a Show are withheld from the focused app. Shift stays
  passive so a Show can draw and pin with it, and another app's double Shift still works; any other
  key during a hold belongs to a different shortcut, so it interrupts the recording, and the
  shortcut waits for release. With Ctrl+Win, an unassigned mask key keeps the released Win key from
  opening Start.
- Return (or keypad Enter on macOS) during a Windows/macOS hold finishes that dictation as a release
  would and marks it to send. Delivery waits up to 5 s for the shortcut's release, then pastes or
  types the text, waits 100 ms for the app to take it, and presses Return once, rechecking that the
  same foreground window (macOS: the same app), not Speakeasy, has focus and the shortcut is
  released. If a check fails, or the Return press is blocked or fails, the text stays and the pill
  says Return wasn't pressed, without offering a second paste. A sent paste keeps the dictation on
  the clipboard rather than restoring the earlier copy. Like the other hold keys, Return is withheld
  whenever the shortcut is down, so on the finishing press of hands-free or a Show it does nothing;
  in practice it finishes the hold without sending. Return with the shortcut up reaches the app.
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
  released within 300 ms and the second pressed within 400 ms of the first's release, drop a pin
  where the pointer was at the second release, unless either tap drew ink or clicked or another
  modifier was held; no pin lands off the recorded window or area. Marks and pins share one
  numbering in the order they began, ① ② ③; each number sits beside what it numbers on the overlay,
  fading with the ink, and in the bundle's keyframes and summary.
- Windows/macOS Escape continues to the focused app. It cancels dictation immediately at any stage,
  but a Show, area choice included, only while the shortcut is held: during the hold that started it
  or the press that would finish it. Otherwise a recording Show ignores Escape, and the tray's
  Cancel Show discards it. An interruption, which is another shortcut or a session change such as
  locking the screen, discards dictation but finishes a recording Show as the shortcut would. Once a
  Show finishes, neither Escape, the tray, nor an interruption discards it while it is saved. Pause,
  Quit, and a settings change can stop pending work, but preserve the finished Show's checkpoint for
  recovery. A committed Show stays saved. Linux uses a reserved Ctrl+Super+Escape chord or an
  explicit command binding; bare Escape is not globally captured. Animation never delays
  cancellation or owns the microphone lifetime.
- On macOS, Secure Keyboard Entry leaves modifier events visible. The pill checks once as the
  microphone opens and, under the slot, names the app macOS reports as holding it. Notes under the
  slot never hide the grille, lock, timer, or countdown; each gets its full time in turn.
- Readiness follows the first captured buffer. Startup feedback does not imply that speech before
  that buffer can be recovered.
- Insertion does not steal focus. OS submission is the observable result; success in every editor
  cannot be inferred from submitting keystrokes.
- On Windows and macOS, a dictation delivered straight after the last one starts with one space when
  the earlier text ended a word and the new one does not open with closing punctuation. Straight
  after means the same window (macOS: the same app), no key typed, and no pointer movement in
  between; macOS also notices a click that leaves the pointer still. Line breaks and scripts written
  without spaces, such as Chinese, Japanese, and Thai, join directly. Text sent with Return or left
  on the clipboard starts fresh. Secure Keyboard Entry hides typing from Speakeasy, so macOS adds no
  space while it is on, and Linux cannot observe typing between dictations, so it never adds one.

## Acceptance

Run `mise run app:demo` to see the actual views with simulated levels and a temporary Show library.
`mise run app:demo-tray` previews the native tray and Settings lifecycle with dictation disabled.
Inspect fast transitions, long processing, cancellation, and reduced motion at several display
scales. See [performance](performance.md#native-acceptance) for live acceptance.
