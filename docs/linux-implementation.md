# Linux implementation

Linux support is experimental. The x86_64 AppImage and bundled tar target glibc 2.35 or later.
Native GNOME, KDE, and X11 desktop acceptance is pending; building the package does not establish
compositor or editor compatibility.

## Shared behavior and desktop adapters

Linux shares everything but its native adapters with Windows and macOS: audio, speech, session
ownership, Settings, the pill, and tray policy. Linux records no Shows: its screen adapter reports
them unavailable. [Architecture](architecture.md) maps the modules and the invariants they share.

Linux selects its input adapter from the host session. An available `DISPLAY` can belong to Xwayland
and does not establish desktop-wide X11 input access. Rendering and input use separate connections
and can use different protocols.

| Desktop path             | Shortcut handling                                                     | Insertion                                                         |
| ------------------------ | --------------------------------------------------------------------- | ----------------------------------------------------------------- |
| X11                      | Passive chord grabs, XI2 release observation                          | Clipboard ownership checks and XTest paste                        |
| Wayland                  | GlobalShortcuts portal with accepted bindings                         | Keyboard-only RemoteDesktop, optional Clipboard portal, and libei |
| Desktop command bindings | `speakeasy --toggle` and `speakeasy --cancel`, as Settings shows them | Explicit automatic or manual-paste preference                     |

Gestures, the default **Ctrl+Super+Space** and reserved **Ctrl+Super+Escape** chords, and command
bindings follow the [interaction contracts](interaction-design.md#interaction-contracts); command
bindings give hands-free only, never hold/release.

Settings shows the actual accepted dictate and cancel bindings. A chord another X11 app already
grabs stops desktop access with an error naming that shortcut, dictation or cancel. X11 releases its
active keyboard grab immediately after a trigger, returning unrelated keys to the focused editor.
Portal activations and deactivations are consumed in message order, with repeated activations
ignored while held. Binding or keyboard map changes require Save and Resume.

## Owned preparation and insertion

Desktop preparation runs outside the UI thread. Shortcut input is enabled after the adapter reports
readiness; capture can proceed while the model loads. Pause cancels pending consent, closes owned
requests and sessions, and awaits native cleanup before replacement. Quit awaits owned insertion and
worker cleanup. Permission loss produces an actionable error rather than a background consent loop.

Portals use the installed `io.github.krvh.speakeasy.desktop` launcher identity. The adapter
registers it before other portal calls; older portal frontends without registration retain their
launcher-derived identity. See the [installation instructions](../README.md#linux-experimental).

Wayland requests keyboard access and optional Clipboard access, except for **Copy for manual
paste**, and never screen capture. Restore credentials are single-use, private 0600 files under the
XDG state directory. They are absent from settings and diagnostics. Frontend restart, session
closure, device pause, and disconnect invalidate usable state.

Insertion follows the shared commit permit and cancellation rules in
[cross-module invariants](architecture.md#cross-module-invariants). Immediately before submitting
input, both paths follow X11/Xwayland focus ancestors to the first window that publishes
`_NET_WM_PID`; when Speakeasy's own windows or a failed lookup hold focus, nothing is submitted.
Paste leaves the text copied for manual paste, and typing reports the target unavailable with the
clipboard untouched. Under Wayland, X focus None means a native Wayland editor took focus, while
PointerRoot stays refused. The portal path queries focus on the owned X clipboard worker, which
keeps cancellation responsive and opens the clipboard only for a copy.

Automatic paste requires released modifiers. X11 queries the X server's key state; Wayland requires
actual EI modifier feedback and a paste key from the advertised layout. Shortcut deactivation alone
is insufficient. Unknown state yields an explicit manual-paste result. X11 automatic paste requires
the first keyboard layout with an unshifted V key; other layouts receive manual-paste guidance.
Clipboard writes are followed by ownership and cancellation checks; cleanup never restores a stale
clipboard payload. Submitted synthetic keys receive release cleanup even after partial failure. X11
queues each paste and its releases before one server synchronization and checks every submission.
Manual copy skips the modifier wait. Wayland serves bounded, asynchronous clipboard transfers while
receiving input and cancellation; clipboard calls through Xwayland run on a serialized, owned
worker. A committed clipboard selection remains available until ownership changes or the service
retires.

Settings' **Insert** control offers **Paste (Ctrl+V)**, **Paste (Ctrl+Shift+V)** for terminals,
**Type (Keep clipboard)**, and **Copy for manual paste**. In the settings file, `manual_paste` takes
precedence over `preserve_clipboard`, which takes precedence over `terminal_paste`. Typing requires
advertised EI text support on Wayland; X11 reports it as unavailable. There is no application-name
guessing, transcript history, automatic provider switch, or repeated ambiguous paste.

## Presentation and lifecycle

The pinned GPUI X11 renderer supplies Settings and the pill, with the native contracts recorded in
[the dependency patch](../vendor/gpui/README.speakeasy.md). Wayland hosts need Xwayland for
presentation. The pill uses notification hints, override-redirect, no keyboard focus, and an empty
input shape before mapping. These hints still need native acceptance for stacking, fullscreen, mixed
scaling, and focus.

Private-Xvfb presentation checks exercise actual Settings rendering, pill startup visibility, input
shape, and simulated show/hide without microphone, shortcuts, or clipboard access. They cover queued
XCB startup events, which must be drained even when checked requests leave no unread bytes in the
socket. Opt-in X11 input tests start their own Xvfb display, then check there that shortcuts survive
a keyboard switch, which X announces as a mapping change, that an actual remap stops them, that a
chord another client already grabs stops them with an error naming the shortcut, and that a first
copy owns the clipboard. Virtual-desktop checks do not establish compositor, desktop input, or
editor compatibility.

The StatusNotifier/DBusMenu adapter reuses shared tray states and menu policy, with subscriptions
limited to relevant host changes. [Tray and Settings](interaction-design.md#tray-and-settings)
covers Linux's Settings at startup and without a tray host.

Animation follows native frames within the shared
[frame budget](performance.md#implementation-constraints).
[Performance profiling](performance.md#native-process-sampling) separates app and worker costs, with
responsiveness and speech quality as constraints.

## Setup and distribution

Configuration uses `$XDG_CONFIG_HOME/speakeasy`, downloads use `$XDG_DATA_HOME/speakeasy`, and
credentials use `$XDG_STATE_HOME/speakeasy`, with standard home-directory defaults. Setup shares
resumable, size- and hash-checked downloads, accelerator selection, locking, extraction, and child
reaping. It preserves complete engine runtime layouts and validates executable permissions.

The pinned upstream Linux CPU engine has a reported illegal-instruction failure on AVX2 hardware
without AVX-512. `doctor` or model loading alone cannot prove inference compatibility. Linux CPU
workers run synthetic inference before readiness; failure stops the worker and explains how to
select a compatible engine. This catches decode failures without supplying a portable engine or
establishing broad CPU coverage. GPU builds also require compatible host drivers.

`package-linux.sh` builds AppImage and bundled tar packages with a glibc 2.35 check for the app and
bundled libraries. `--native-tar` produces an unbundled development archive for the builder's ABI.
The release workflow builds Linux on Ubuntu 22.04. Local native graphics checks run the built
binaries and both packaged launchers in an Ubuntu 24.04 container with stock Mesa and a private
virtual display. They cover rendered Settings, simulated pill show/hide, and hidden-window display
disconnect; the tar launcher checks startup and Settings rendering only. The software driver needs
the X11 presentation-depth fix included in
[Mesa 24.1](https://docs.mesa3d.org/relnotes/24.1.0.html); Ubuntu 22.04's Mesa 23 can present black
windows under Xvfb without DRI3. This fixture limitation does not establish compatibility of
hardware drivers or desktop compositors. Models and engines remain separate downloads.

## Native interface contracts

Treat rendering, shortcuts, modifier feedback, and insertion as separate capabilities. Xwayland's
isolation means an X11 window and `DISPLAY` do not prove access to a focused Wayland editor. See the
[Xwayland architecture](https://wayland.freedesktop.org/docs/book/Xwayland.html) and
[XI2 protocol](https://xorg.freedesktop.org/archive/X11R7.7/doc/inputproto/XI2proto.txt).

The
[GlobalShortcuts interface](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)
returns accepted bindings, their descriptions, and activation/deactivation events. It does not
expose a passive Escape or physical modifier stream. Follow the
[shortcut syntax](https://specifications.freedesktop.org/shortcuts/latest/) and probe actual
interfaces and permissions. InputCapture's compositor-controlled pointer barriers do not satisfy
passive background keyboard observation; see the
[InputCapture interface](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html).

[RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
owns keyboard consent, pending requests, restore tokens, and the EI connection. After
`ConnectToEIS`, D-Bus `Notify*` injection cannot be used on that session.
[Clipboard](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html)
extends an approved session: request it before Start and check `clipboard_enabled`. Its transfer
serial identifies a request rather than a global ownership sequence. Keep owned selections and
transfer descriptors alive as required, retire stale transfers, and preserve a newer clipboard
owner.

EI sender feedback reports logical XKB masks, not a passive physical key stream. Parse the device
keymap and inspect relevant modifier indices; Caps/Num Lock alone must not block insertion. Missing,
paused, or disconnected feedback is unknown, and sender sync does not order the separate shortcut
D-Bus stream. Compositor support needs native validation; compare the pinned
[Mutter 50 implementation](https://raw.githubusercontent.com/GNOME/mutter/50.0/src/backends/meta-eis-client.c),
[Mutter 49 implementation](https://raw.githubusercontent.com/GNOME/mutter/49.0/src/backends/meta-eis-client.c),
and [KDE modifier-feedback change](https://kde.org/announcements/changelogs/plasma/6/6.6.5-6.6.90/).
The owning contracts are the
[EI keyboard protocol](https://libinput.pages.freedesktop.org/libei/interfaces/ei_keyboard/index.html)
and [XKB state interpretation](https://xkbcommon.org/doc/current/group__state.html).

The dynamic adapter follows the
[libei C header](https://libinput.pages.freedesktop.org/libei/api/libei_8h_source.html), and its
code states the ownership and lifetime rules it relies on. Optional text symbols do not prove
compositor or editor support. See
[EI text](https://libinput.pages.freedesktop.org/libei/interfaces/ei_text/index.html),
[seat binding](https://libinput.pages.freedesktop.org/libei/api/group__libei-seat.html),
[device keymaps](https://libinput.pages.freedesktop.org/libei/api/group__libei-device.html), and
[sender lifetime](https://libinput.pages.freedesktop.org/libei/api/group__libei-sender.html).

GPUI's Wayland constructor assigns an xdg-toplevel role before returning. A post-creation native
adapter cannot turn that surface into layer-shell because the role lasts for its lifetime. A native
Wayland pill needs a pre-role construction boundary, so Linux presents through X11/Xwayland. See
[GPUI's constructor](https://docs.rs/crate/gpui/0.2.2/source/src/platform/linux/wayland/window.rs),
[Wayland roles](https://wayland.freedesktop.org/docs/html/apa.html#protocol-spec-wl_surface), and
[layer-shell](https://raw.githubusercontent.com/swaywm/wlr-protocols/master/unstable/wlr-layer-shell-unstable-v1.xml).
X11 pill hints follow [EWMH](https://specifications.freedesktop.org/wm/latest/ar01s05.html) and the
[X Shape protocol](https://xorg.freedesktop.org/archive/X11R7.7/doc/xextproto/shape.html).

The
[host Registry contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.host.portal.Registry.html)
describes registration for unsandboxed applications. Package and engine ABI checks are distinct: the
pinned
[NeMo requirements](https://raw.githubusercontent.com/NVIDIA/NeMo-Speech.cpp/v0.2.0/docs/install.md)
describe host GPU drivers, while a
[CPU artifact report](https://github.com/NVIDIA/NeMo-Speech.cpp/issues/23) shows why model loading
does not establish inference compatibility. Preserve the
[installer layout](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/v0.2.0/scripts/install.sh) and
[RPATH definitions](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/v0.2.0/CMakeLists.txt);
`setup.rs` owns the model's URL and checksum, and `setup/engine.rs` owns the engines'.

## Native acceptance still required

| Environment                      | Required checks                                                                                                                | Status  |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | ------- |
| X11 desktop                      | Hold/double-tap, keymaps, clipboard-manager races, focus and click-through                                                     | Pending |
| GNOME Wayland                    | Accepted bindings, EI modifier/text support, portal denial/revocation, no-tray behavior, focus loss to a native Wayland editor | Pending |
| KDE Wayland                      | Accepted bindings, EI state, permission recovery, tray and fullscreen behavior, focus loss to a native Wayland editor          | Pending |
| Desktop without suitable portals | Command bindings, manual paste, launcher reopening                                                                             | Pending |
| Baseline distro and CPU          | Public-fixture inference, host driver requirements, engine instruction set                                                     | Pending |

Also verify mixed-DPI placement, display removal, cancellation cleanup, native Wayland and X11
editors, terminals, and memory/resource return after Pause and Quit. Measure startup, idle, capture,
inference, and displayed frame pacing under competing load.

WSL checks do not establish desktop routing or native audio quality.
