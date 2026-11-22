# Linux implementation

Linux shares capture, speech, the session machine, and the shell. It reads saved Shows. For
installation and bindings see the [Linux section of the README](../README.md#linux-experimental).

## Desktop and insertion

Rendering and input select their adapters independently.
[Display selection](../crates/platform/src/linux/display_server.rs) probes layer shell before GPUI
opens a window. Without it GPUI uses X11 or Xwayland. An available `DISPLAY` can be Xwayland even
when the input adapter needs Wayland portal permissions. The pill uses a passive layer surface or an
X11 notification window with an empty input shape. Show and hide through the same GPUI display
connection. Hiding a Wayland pill unmaps its surface so the compositor can choose the output again
on show.

[Linux desktop](../crates/platform/src/linux.rs) routes X11 to passive chord grabs and XI2 release
observation. Wayland uses GlobalShortcuts for accepted bindings and keyboard-only RemoteDesktop for
libei, with optional Clipboard access. Desktop bindings bypass shortcut registration. Manual paste
also bypasses keyboard consent. Restore tokens belong in the XDG state directory and rotate when the
desktop returns a replacement. Pause must retire native requests and sessions before resuming.

Both input paths check GPUI's own-window activation alongside X11/Xwayland focus. On layer shell the
GPUI report is necessary to identify Speakeasy's own windows. Wayland modifier readiness comes from
EI. X11 reads the X server's key state. Shortcut deactivation alone cannot stand in for modifier
release. Direct typing requires an EI text device. See
[native contracts](native-contracts.md#linux-desktop-and-transfer-ownership) for ordering and
transfer lifetime.

The tray uses StatusNotifier and DBusMenu. Without a host, closing Settings quits. With one, closing
Settings hides the view without ending a setup or save.

## Setup and distribution

Configuration uses `$XDG_CONFIG_HOME/speakeasy`, downloads use `$XDG_DATA_HOME/speakeasy`, and
portal restore tokens use `$XDG_STATE_HOME/speakeasy`, with home directory defaults. Setup uses the
shared download and extraction path and preserves the engine's complete runtime layout.

The pinned Linux CPU engine has an illegal-instruction failure on some CPUs without AVX-512. Model
loading alone cannot catch it. [Local speech](../crates/app/src/local_speech.rs) runs synthetic
inference before reporting Linux CPU readiness and names the remedy on failure.

[Linux packaging](../scripts/package-linux.sh) checks glibc 2.35 for the app and bundled libraries.
It relies on RUNPATH to locate bundled libraries and keeps the host environment for desktop
services. The release builder uses Ubuntu 22.04. The opt-in
[rendering check](../scripts/check-rendering-linux.sh) runs the binaries with stock Mesa in an
Ubuntu 26.04 container. The private display needs Mesa's X11 presentation-depth fix, which the older
builder driver lacks.

## Native acceptance still required

| Desktop                  | Checks                                                                                               |
| ------------------------ | ---------------------------------------------------------------------------------------------------- |
| X11                      | Held shortcuts, layout changes, automatic and manual paste, focus, and click-through                 |
| GNOME Wayland            | Accepted shortcuts, EI modifier and text support, denial and revocation, focus, and no-tray behavior |
| KDE Wayland              | Layer-shell pill over fullscreen windows, EI state, permission recovery, and tray reopen             |
| sway, Hyprland, niri     | Pill placement and idle unmapping, output hotplug, and fullscreen presentation                       |
| Desktop command bindings | Hands-free, cancel, manual paste, and launcher reopening                                             |

Default portal transfer tests use a private `dbus-daemon`. The private-Xvfb checks exercise owned
window rendering and X11 paths. Real desktops are needed for compositor and editor acceptance.
Confirm capture with PipeWire's ALSA plugin where the Realtime portal offers no budget. See
[the realtime opt-out](native-contracts.md#linux-realtime-opt-out).
