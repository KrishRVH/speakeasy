# Release notes

Speakeasy 0.13.0 leaves Escape to your apps during hands-free dictation, keeps the shortcut and your
words working in more places on Windows, and keeps agents' Show skills current across updates.

- **Escape during hands-free.** On Windows and Mac, Escape reaches the focused app and no longer
  throws away hands-free dictation. To cancel, hold the shortcut and press Escape, or choose
  **Cancel dictation** in the tray. Hands-free dictation finishes on the release after a fresh
  press, as a Show does: Return on that press finishes and sends, and S, W, or A turns it into a
  Show. Escape alone still cancels hold-to-talk and a dictation being transcribed, and Linux keeps
  Ctrl+Super+Escape.
- **Shortcut in front of more apps.** Some Windows apps, such as Tauri 2 apps, kept Speakeasy from
  hearing its shortcut while they were in front. Speakeasy now reinstalls its keyboard hook each
  time another app comes forward.
- **Held keys and remappers.** A key held from before a hold, such as the source key a remapper
  turns into your talk key, repeats through the hold instead of cancelling it.
- **Words kept when a paste would be lost.** On Windows, dictating while the desktop or taskbar has
  focus leaves the words copied with a note saying where to paste them. macOS 27.3 moves the grant
  to send keystrokes to **System & App Protection Override**; there, Speakeasy checks it before
  pasting, leaves the words copied with a note naming the pane, and asks macOS to list Speakeasy.
- **Windows microphone after a Show.** The default microphone and cues keep opening after a Show.
  Before, a device change could make them fail until Speakeasy restarted.
- **OBS and Whisper.** OBS's capture layer stays out of the Windows speech engine. Whisper large-v3
  and large-v3-turbo sometimes invent “Продолжение следует...” or Russian subtitle credits from
  brief audio; Speakeasy drops those as it does the other known credits.
- **Shows for agents.** Export opens in Downloads instead of the hidden app-data folder. A summary
  pasted without **Send to agent** names Speakeasy and points the agent to the speakeasy-show skill.
  `show skill --install` replaces any copy already there, and the first Show command after each
  update rewrites the copies it installed. Run it once more for a copy installed by 0.12.1 or
  earlier. On Windows, an update ends running Show MCP servers so it can replace Speakeasy;
  reconnect the agent's Speakeasy server afterward.
- **Clearer messages.** Messages from the app, CLI, and MCP say “doesn't”, “isn't”, and “didn't”,
  and a failed save no longer leaves an Undo for a deleted preset.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows and their
saved audio expire after seven days unless kept; pending recovery audio always expires seven days
after recording.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux doesn't record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
