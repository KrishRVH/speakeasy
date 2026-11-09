# Release notes

Speakeasy 0.11.0 frames screen-Show keyframes around the work you did, deletes saved Shows, and
names a microphone that sends only silence.

- **Keyframes framed around your work.** A screen Show crops each keyframe to where you worked: your
  ink, pins, clicks, and drags, and what they changed. When that input falls in one or two windows,
  the keyframe frames those windows whole, with their app's open menus and dialogs, so a label
  beside the control you clicked stays in frame. With nothing to point at, the front window frames
  the keyframe. A crop that would cover most of the screen stays whole, and window and area Shows
  keep the framing you chose. To frame windows, a Show notes where windows sit and which belong to
  the same app, never their titles, app names, or contents, and only its private recovery checkpoint
  keeps those positions. In the Shows window, **Whole screen** beside a cropped keyframe's caption
  opens the retained frame at that moment.
- **Show format.** `show.json` keyframes gain an optional `crop`, given as left, top, right, and
  bottom fractions of the screen, and `narrationLost` can be `audioSilent`. The summary,
  `show inspect`, the MCP keyframe tool, and the review caption name cropped frames. Tools that read
  `show.json` should accept both.
- **Delete Shows.** **More actions → Delete Show…** permanently deletes the selected Show, including
  a kept or damaged one, with its saved files and recovery audio. Exported copies stay.
- **Silent microphones.** A muted or privacy-blocked microphone that sends only digital silence now
  gets an error naming the mute switch, privacy settings, and the microphone choice, in place of "No
  speech detected". A Show still saves without narration and says why. A microphone that idles at a
  constant offset no longer pins the level meter or defeats silence trimming.
- **Mac menu bar over full-screen apps.** The menu bar menu opens while a full-screen app owns the
  Space.
- **Clearer Shows window.** The Shows window changes its library one step at a time, and each button
  is enabled only when its action can run: **Keep Show** waits for a recovery or discard, and
  **Refresh** for a deletion, where both used to look available and do nothing. Escape dismisses an
  open Discard confirmation in place of closing the window.
- **Smaller fixes.** The exported archive's guide opens the contact sheets first, like the summary
  and Send to agent. Linux insertion errors name the **Insert text by** control. A launch refuses a
  repeated `--config` or both `--toggle` and `--cancel`. Packages no longer build image decoders
  Speakeasy never uses.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows expire
after seven days unless kept; keeping a Show never extends its audio retention.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
