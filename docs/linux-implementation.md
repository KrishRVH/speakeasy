# Linux implementation

Linux support is experimental. The x86_64 AppImage and bundled tar target glibc
2.35 or later. Native GNOME, KDE, and X11 desktop acceptance is pending; building
the package does not establish compositor or editor compatibility.
[Platform research](linux-platform-research.md) records the primary-source
constraints, including Wispr Flow's lack of a native Linux app.

## Shared behavior and desktop adapters

The three platforms share pure gestures and motion in `core`, plus audio,
speech, session ownership, Settings, the pill, and tray policy in `app`. Native
mechanics belong to `platform`; [architecture](architecture.md) describes the
module boundaries.

Linux selects its input adapter from the host session. An available `DISPLAY`
can belong to Xwayland and does not establish desktop-wide X11 input access.
Rendering and input use separate connections and can use different protocols.

| Desktop path | Shortcut handling | Insertion |
| --- | --- | --- |
| X11 | Passive chord grabs, XI2 release observation | Clipboard ownership checks and XTest paste |
| Wayland | GlobalShortcuts portal with accepted bindings | Keyboard-only RemoteDesktop, optional Clipboard portal, and libei |
| Desktop command bindings | Fixed `speakeasy --toggle` and `speakeasy --cancel` commands | Explicit automatic or manual-paste preference |

The default dictate chord is **Ctrl+Super+Space**. Hold/release finishes a
recording; double-tap enables hands-free, and another press finishes it.
**Ctrl+Super+Escape** is the reserved cancel chord. Bare Escape is not globally
captured on Linux. Desktop command bindings start/finish hands-free; they do not
provide hold/release. The five-minute limit and gesture timings remain shared.

Settings shows the actual accepted dictate and cancel bindings. X11 releases
its active keyboard grab immediately after a trigger, returning unrelated keys
to the focused editor. Portal activations and deactivations are consumed in
message order, with repeated activations ignored while held. Binding or keyboard
map changes require Save and Resume.

## Owned preparation and insertion

Desktop preparation runs outside the UI thread. Shortcut input is enabled after
the adapter reports readiness; capture can proceed while the model loads.
Pause cancels pending consent, closes owned requests and sessions, and awaits
native cleanup before replacement. Quit awaits owned insertion and worker cleanup.
Permission loss produces an actionable error rather than a background consent loop.

Portals use the installed `io.github.krvh.speakeasy.desktop` launcher identity.
The adapter registers it before other portal calls; older portal frontends
without registration retain their launcher-derived identity. See the
[installation instructions](../README.md#linux-experimental).

Wayland requests keyboard access, optional Clipboard access, and no screen
capture. Restore credentials are single-use, private 0600 files under the XDG
state directory. They are absent from settings and diagnostics. Frontend restart,
session closure, device pause, and disconnect invalidate usable state.

Every insertion owns a generation-bound commit permit and carries its session
ID. Cancellation or a later recording cannot authorize an old paste or accept
its result. The session owner continues receiving input while native preparation
waits. Windows/macOS serialize native insertion calls on a blocking executor;
their gesture hooks retain passive Escape.

Automatic paste requires released modifiers. X11 queries physical key state;
Wayland requires actual EI modifier feedback and a paste key from the advertised
layout. Shortcut deactivation alone is insufficient. Unknown state yields an
explicit manual-paste result. X11 automatic paste requires the first keyboard
layout with an unshifted V key; other layouts receive manual-paste guidance.
Clipboard writes are followed by ownership and
cancellation checks; cleanup never restores a stale clipboard payload. Submitted
synthetic keys receive release cleanup even after partial failure.

**Copy for manual paste** is an explicit option. **Keep clipboard** requires
advertised EI text support on Wayland; X11 reports direct insertion as unavailable.
Terminal users can select **Ctrl+Shift+V**. There is no application-name guessing,
transcript history, automatic provider switch, or repeated ambiguous paste.

## Presentation and lifecycle

Official GPUI's X11 renderer supplies Settings and the pill. Wayland hosts need
Xwayland for presentation. The pill uses notification hints, override-redirect,
no keyboard focus, and an empty input shape before mapping. These hints still
need native acceptance for stacking, fullscreen, mixed scaling, and focus.

The StatusNotifier/DBusMenu adapter reuses shared tray states and menu policy,
with subscriptions limited to relevant host changes. Without a tray host,
Settings remains discoverable and closing it quits. Linux Settings opens at
startup so permission and tray state remain accessible.

Animation follows native frames with the shared 200-FPS ceiling. Meter updates
coalesce into the pending frame, session changes redraw immediately, and settled
hidden views request no animation frames. [Performance profiling](performance.md#linux-port-and-shared-audio-pass)
separates app and worker costs, with responsiveness and speech quality as constraints.

## Setup and distribution

Configuration uses `$XDG_CONFIG_HOME/speakeasy`, downloads use
`$XDG_DATA_HOME/speakeasy`, and credentials use `$XDG_STATE_HOME/speakeasy`, with
standard home-directory defaults. Setup shares resumable, size- and hash-checked
downloads, accelerator selection, locking, extraction, and child reaping. It
preserves complete engine runtime layouts and validates executable permissions.

The pinned upstream Linux CPU engine has a reported illegal-instruction failure
on AVX2 hardware without AVX-512. `doctor` or model loading alone cannot prove
inference compatibility. Linux CPU workers run synthetic inference before
readiness; failure stops the worker and explains how to select a compatible
engine. This catches decode failures without supplying a portable engine or
establishing broad CPU coverage. GPU builds also require compatible host drivers.

`package-linux.sh` builds AppImage and bundled tar packages with a glibc 2.35
check for the app and bundled libraries. `--native-tar` produces an unbundled
development archive for the builder's ABI. The release workflow builds Linux
on Ubuntu 22.04, runs the required Cargo checks, and checks both packaged launchers
without opening desktop resources. Models and engines remain separate downloads.

## Native acceptance still required

| Environment | Required checks | Status |
| --- | --- | --- |
| X11 desktop | Hold/double-tap, keymaps, clipboard-manager races, focus and click-through | Pending |
| GNOME Wayland | Accepted bindings, EI modifier/text support, portal denial/revocation, no-tray behavior | Pending |
| KDE Wayland | Accepted bindings, EI state, permission recovery, tray and fullscreen behavior | Pending |
| Desktop without suitable portals | Command bindings, manual paste, launcher reopening | Pending |
| Baseline distro and CPU | Public-fixture inference, host driver requirements, engine instruction set | Pending |

Also verify mixed-DPI placement, display removal, cancellation cleanup, native
Wayland and X11 editors, terminals, and memory/resource return after Pause and
Quit. Measure startup, idle, capture, inference, and displayed frame pacing under
competing load. Windows/macOS insertion needs native acceptance after the shared
asynchronous ownership change.

Default tests use fakes or public fixtures. Live microphone, shortcuts, clipboard,
and focused-editor acceptance require explicit opt-in. WSL checks do not establish
desktop routing or native audio quality.
