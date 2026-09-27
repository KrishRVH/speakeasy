Speakeasy for Windows — local dictation

This bundle includes the app, Parakeet engine and model. It requires an NVIDIA
GPU and was verified on an RTX 4090. It works offline without an account.

1. Extract the entire Speakeasy folder to a Windows drive. Keep its files together.
2. Close any running Speakeasy instance, then double-click Speakeasy.cmd.
3. Wait for the local model to load. Hold Ctrl+Alt+Space, speak, then release.

Double-tap the shortcut for hands-free dictation; tap again to finish.
Escape cancels. Recordings stop after five minutes.
The microphone starts only when you trigger dictation.

The tray menu opens Settings, pauses dictation or quits. Closing Settings keeps
the app in the tray. Pause releases the loaded model. Settings are stored in this
folder.

Standard insertion puts the transcript on the clipboard and sends paste to the
focused app. Preserve clipboard uses direct text input, which some editors reject.
Elevated apps and protected fields may block insertion.

Parakeet detects language automatically. After a long recording, the engine may
retain about 3.8 GB of GPU memory until paused. First startup can take several
seconds. If your microphone format is unsupported, use a 48 kHz Windows setting.

For a preview without microphone, global shortcut or clipboard access, run:
Speakeasy.cmd --demo

Automated checks used public audio and simulated capture/insertion. Actual
microphone-to-editor behavior has not been manually verified on this machine.
See MODEL-NOTICE.txt and engine/licenses for included component attribution.
