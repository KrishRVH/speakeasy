# Release notes

Speakeasy 0.10.0 runs natively on Wayland desktops with layer shell, lets the pill's waveform follow
your voice, and makes Settings and the Shows window easier to use with a keyboard or screen reader.

**Breaking change:** `speakeasy show frames` and `speakeasy show frame` refuse a focus value that
begins with `-`, such as `--phrase --json`, instead of reading it as a phrase, as every other option
already does. Until 1.0.0, Speakeasy supports its current formats and settings only, with no
migration or compatibility paths.

- **Native Wayland.** On KDE Plasma, sway, Hyprland, niri, and other compositors with layer shell,
  Speakeasy runs as a Wayland client. The pill appears at the bottom of the focused display, never
  takes focus or clicks, leaves the screen entirely while idle, and comes back after its display is
  unplugged. Closing Settings to the tray hides it without stopping a model or update download.
  GNOME, WSLg, and other desktops without layer shell keep using X11 through Xwayland. With neither
  available, Speakeasy stops with a message that says what to enable.
- **A waveform that follows your voice.** The pill's bars rise with each syllable and fall in
  pauses, scaled to how loudly you speak rather than to a fixed range.
- **Screen readers.** On Windows, text ranges a screen reader held before Settings was hidden stop
  working instead of reading the reopened window, a request to scroll text into view lands even if
  the field resizes first, and closing a window disconnects it from UI Automation. In the Shows
  window, the library is a list of selectable rows and each narration clause is a labeled button. On
  macOS and Linux, Settings now publishes the same accessibility tree as on Windows.
- **Shows window.** The selected Show and the selected frame stay scrolled into view. On Linux,
  revealing an open Shows window raises it. A Show that cannot open says its narration is
  unavailable instead of loading forever.
- **Clearer errors.** Exporting into a missing folder, control commands with a bad session token,
  preset name, or style, and `speakeasy show` options now say which rule was broken. On macOS,
  Settings refuses an engine program without execute permission instead of failing when dictation
  starts.

Hold-to-talk, double-tap hands-free, and the five-minute recording limit remain. Recognition stays
local, with no accounts, telemetry, dictation history, or automatic uploads. Managed Shows expire
after seven days unless kept; keeping a Show never extends its audio retention.

Packages support Windows 11 x64 and macOS 14 or newer on Apple silicon. Experimental Linux x86_64
AppImage and bundled tar packages require glibc 2.35 or newer; Linux does not record Shows. Attached
SHA-256 checksums cover the packages, and signed metadata supports Windows update preparation.
