# speakeasy on this PC

Open **speakeasy** using the desktop shortcut. The installed app is at
`C:\Users\Krish\Apps\speakeasy\speakeasy.exe` and will already be in the
system tray after setup.

- Hold **Ctrl+Alt+Space**, speak, and release all three keys to paste.
- Double-tap the shortcut for hands-free recording; tap once more to finish.
- **Esc** cancels and still reaches the app you are typing in.
- Double-click the tray icon for Preferences. Close the dashboard to return
  to background operation. The tray's **Launch at login** switch is optional.

Configuration and models are in `C:\Users\Krish\.speakeasy`. The installation
uses this independent user folder so it works both inside and outside the
development app. The installed `config-location.txt` remembers that location.

This machine is configured for GPU-backed **Whisper medium.en** and local
**Qwen2.5 3B Instruct** cleanup. Both models are installed and warm in memory
while the app is running. No API key or internet connection is needed.

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
application blocks automatic paste, the transcript remains copied for Ctrl+V.
For full settings and Windows keyboard details, see the [README](../README.md).
