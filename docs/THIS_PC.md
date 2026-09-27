# Recorded .NET workstation setup

These notes describe the .NET installation recorded on September 9, 2026. Paths,
devices and saved preferences below refer to that installation. For the current
Rust Windows bundle, use the [README](../README.md#optional-parakeet-engine).

Open **speakeasy** using the desktop shortcut. The installed app is at
`C:\Users\Krish\Apps\speakeasy\speakeasy.exe` and will already be in the
system tray after setup.

- Hold **Ctrl+Alt+Space**, speak, and release all three keys to paste.
- Double-tap the shortcut for hands-free recording; tap once more to finish.
- **Esc** cancels and still reaches the app you are typing in.
- Double-click the tray icon to open the dashboard. **Try dictation** gives you
  a temporary text box with Start and Finish buttons. **Preferences → Check
  microphone** checks the selected input for ten seconds and discards its audio.
- Close the dashboard to return to background operation. The tray's
  **Launch at login** switch is optional.

Configuration and models are in `C:\Users\Krish\.speakeasy`. The installation
uses this independent user folder so it works both inside and outside the
development app. The installed `config-location.txt` remembers that location.

This machine is configured for GPU-backed **Whisper medium.en** and local
**Qwen2.5 3B Instruct** cleanup. Both models are installed and warm in memory
while the app is running. No API key or internet connection is needed.

Normal settings use the Windows default microphone, a five-minute recording
limit, and leave the latest transcript on the clipboard. Preferences lets you
select a specific microphone or restore your previous clipboard after paste.

For Moonlight, keyboard delivery and microphone delivery are separate. The
host already has Voicemeeter and VB-CABLE; route the client's microphone into
one of their Windows recording inputs, then select it in Preferences. Follow
the [remote microphone guide](remote-dictation.md). The host's cable capture
and insertion path has been exercised with synthetic speech; a microphone
sender on your remote client still needs to be connected.

To use a cloud provider later, edit `C:\Users\Krish\.speakeasy\.env`, choose
the provider in Preferences, and reload settings. **Advanced settings file**
opens `C:\Users\Krish\.speakeasy\settings.json`.

The source repository is `C:\Users\Krish\devr\speakeasy`. To run that source
against your installed configuration:

```powershell
dotnet run --project src/Speakeasy.App -- --config-dir C:\Users\Krish\.speakeasy --settings
```

To install a later source change to the same location:

```powershell
./scripts/install.ps1 -InstallDirectory C:\Users\Krish\Apps\speakeasy -ConfigDirectory C:\Users\Krish\.speakeasy -DesktopShortcut -Launch
```

Your microphone privacy settings currently allow access. If a particular
application blocks the normal clipboard paste path, the transcript remains
copied for Ctrl+V. Direct insertion instead preserves the previous clipboard
and reports the problem without claiming that the transcript was copied.
For full settings and Windows keyboard details, see the [README](../README.md).
The [verification notes](VERIFIED.md) distinguish native acceptance, automated
checks, measured model latency, and the remaining physical/remote checks.
