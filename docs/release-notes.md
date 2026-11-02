# Release notes

Speakeasy 0.9.0 brings Settings and Shows into one design, guides your first dictation through
private practice, makes the speech model one plain choice, and hands Shows to any coding agent.

**Breaking changes:** `speakeasy show sessions`, `speakeasy show send`, and **Send to OpenCode** in
Shows are removed; **Send to agent** replaces them. Until 1.0.0, Speakeasy supports its current
formats and settings only, with no migration or compatibility paths. A settings file this version
cannot read offers **Start fresh…** instead.

- **Send to agent.** A saved Show's primary action copies a short prompt that points Claude Code or
  any coding agent that reads local files at the Show's folder, its contact sheets in order, and
  `summary.md` with your edits. The prompt holds paths, not narration or pixels, and Speakeasy
  contacts no agent. The bundled agent guide explains how to read it.
- **A guided first dictation.** The first launch walks through local speech setup, dictation access,
  and private practice that waits for recognized words before the ready screen. Practice never
  pastes, copies, or saves. **Skip setup** keeps downloads running, unfinished setup opens again on
  launch, and **General → Setup guide → Replay** brings the guide back.
- **Settings that apply as you go.** Settings splits into Dictation, Words, and General and applies
  each change as you make it. A change made while dictation is paused stays paused, and a failed
  save returns the window to the settings actually in use. Presets show **In use**, apply with
  **Use**, and update or delete from their **⋯** menu; corrections edit in place and recognition
  hints appear as chips.
- **One speech model choice.** **Dictation → Speech model** offers Parakeet v3, Nemotron 3.5, or
  your own whisper.cpp or NeMo-Speech build, each saying what it is for and whether it is selected,
  downloaded, or a download away. Choosing Parakeet or Nemotron downloads what is missing and
  switches when it finishes; choosing the selected model again re-checks and repairs its files. Your
  own engine switches once its program and model are chosen.
- **Start over.** **General → Start over…** deletes downloaded engines, models, and updates, then
  the settings file, keeps every Show, and sets up Parakeet again; as on a new install, finishing
  that setup turns dictation on. It refuses while another copy is setting up or preparing an update,
  and a failed reset keeps your settings.
- **One design.** Settings, Shows, the pill, and the tray share one set of controls, with focus
  rings that appear after keyboard use. The Shows window gains a library header, tagged rows, a
  **More actions** menu, and a separate confirmation before discarding an interrupted Show.
  Destructive actions use the attention color, and action menus are exposed to assistive technology
  as menus.
- **Fixes.** A control command that cannot find its settings folder now reports `unknown` with exit
  status 2 instead of claiming the app refused it. The Mac setup guide shows the shortcut you chose
  rather than Fn.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows expire
after seven days unless kept; keeping a Show never extends its audio retention.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
