Speakeasy for Windows — local dictation

This bundle includes the app, Parakeet engine and model. It requires an NVIDIA
GPU and was verified on an RTX 4090. It works offline without an account.

1. Extract the entire Speakeasy folder to a Windows drive. Keep its files together.
2. Double-click Speakeasy.cmd. The app starts in the system tray.
3. Open Settings from the tray to check readiness or select your microphone.
   Wait for the model to load, then hold Ctrl+Alt+Space, speak, and release.

Double-tap the shortcut for hands-free dictation; tap again to finish.
Escape cancels. Recordings stop after five minutes.
The microphone starts only when you trigger dictation.

The tray menu opens Settings, pauses or resumes dictation, or quits. Closing or
minimizing Settings hides its window and preserves unsaved edits. Reopening
Speakeasy restores that window. Pause releases the loaded model. Settings are
stored in this folder. Quit the app before replacing files with an updated build.

Standard insertion puts the transcript on the clipboard and sends paste to the
focused app. Preserve clipboard uses direct text input, which some editors reject.
Elevated apps and protected fields may block insertion.

Parakeet detects language automatically. After a long recording, the engine may
retain about 3.8 GB of GPU memory until paused. First startup can take several
seconds. If your microphone format is unsupported, use a 48 kHz Windows setting.

For a preview without microphone, global shortcut or clipboard access, run:
Speakeasy.cmd --demo

To also preview the tray and Settings hide/reopen behavior, run:
Speakeasy.cmd --demo-tray
Quit the regular app first; each folder runs one tray instance at a time.

Automated checks used public audio and simulated capture/insertion. Actual
microphone-to-editor behavior has not been manually verified on this machine.
See MODEL-NOTICE.txt and engine/licenses for included component attribution.
