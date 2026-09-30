# Linux platform research

Checked 29 September 2026 against Speakeasy v0.1.1, GPUI 0.2.2,
tray-icon 0.25.1, primary specifications, and compositor source. This is research
for a port, not a statement of implemented Linux support.

**Proposed choice:** keep the session owner and pure gesture model portable;
separate desktop input and insertion from window presentation. Prototype the
existing GPUI X11 renderer on X11 and Xwayland first. Enable automatic insertion
only when the selected desktop adapter can establish safe modifier state.

## Desktop routing and gestures

**Fact:** rootless Xwayland integrates X11 windows into a Wayland desktop through
the compositor's X window manager. It isolates X11 clients from native Wayland
clients and does not promise complete X11 compatibility. **Inference:** `DISPLAY`
and an X11 GPUI window do not establish that desktop-wide X11 input observation
or injection can reach the focused Wayland application. Detect the host session
separately from the rendering connection. [Xwayland architecture](https://wayland.freedesktop.org/docs/book/Xwayland.html)

**Fact:** XInput2 defines raw key press and release events. **Proposed choice:**
use an owned X11 adapter for modifier gestures and passive Escape on a genuine
X11 desktop, without grabbing Escape. Keep this route separate from Wayland
portals. [XInput2 protocol](https://xorg.freedesktop.org/archive/X11R7.7/doc/inputproto/XI2proto.txt)

**Fact:** GlobalShortcuts reports action `Activated` and `Deactivated`, lets the
user configure bindings, and returns the actual bound subset and descriptions.
It exposes no passive raw Escape, Space, or modifier stream. Its preferred
shortcut syntax combines modifiers with a key identifier. **Proposed choice:**
offer configurable Linux shortcuts for hold, hands-free/toggle, and cancel;
preserve double-tap where release events are usable. A registered cancel action
must not be described as passive Escape. [GlobalShortcuts API](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html),
[shortcut syntax](https://specifications.freedesktop.org/shortcuts/latest/)

**Fact:** the wlr portal advertises only Screenshot and ScreenCast.
**Inference:** a wlroots desktop or installed portal frontend does not imply
GlobalShortcuts or RemoteDesktop support. **Proposed choice:** probe actual
interfaces, versions, bindings, and permissions; expose a toggle/UI compatibility
path when a hold action is unavailable. [wlr portal interfaces](https://raw.githubusercontent.com/emersion/xdg-desktop-portal-wlr/master/wlr.portal)

## Paste permission and clipboard ownership

**Fact:** RemoteDesktop requests device types and requires a user-approved start.
Request keyboard only; screen capture is a separate opt-in integration.
`ConnectToEIS` supplies a sender connection; once connected, the D-Bus `Notify*`
input methods cannot be used on that session. Restore tokens are single-use and
permissions may be withdrawn. **Proposed choice:** own the portal session,
connection, pending requests, and injected key releases; surface permission loss
through the session owner. [RemoteDesktop API](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)

**Fact:** the Clipboard portal extends a RemoteDesktop or InputCapture session;
request access before start and check `clipboard_enabled`. Clipboard data uses
MIME ownership, transfer requests, file descriptors, and transfer completion.
`SelectionOwnerChanged` includes `session_is_owner`; transfer serials identify
requests, not a Windows-style global clipboard sequence. **Proposed choice:**
retain an owned selection service while text should remain pasteable, retire
stale transfers, and preserve a newer owner's selection. Avoid promising that
closing a portal session retains its clipboard data. [Clipboard API](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html)

**Fact:** D-Bus injection offers keycodes/keysyms. Current EI also defines an
optional `ei_text.utf8` interface, with at most 254 bytes per request and one
request per frame; it may deliver through an input method rather than keys.
**Proposed choice:** expose **Keep clipboard** only for a verified text capability,
with documented editor limits; do not assume arbitrary Unicode works through
keycodes or keysyms. Broad compositor support for EI text remains unverified.
[RemoteDesktop keyboard methods](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html#org-freedesktop-portal-remotedesktop-notifykeyboardkeysym),
[EI text protocol](https://libinput.pages.freedesktop.org/libei/interfaces/ei_text/index.html)

## Modifier state before submission

**Fact:** shortcut deactivation reports only that the action is inactive.
**Inference:** releasing the shortcut's main key can end a hold while Ctrl or
Super remains down; deactivation alone cannot authorize a paste.
[GlobalShortcuts deactivation](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html#org-freedesktop-portal-globalshortcuts-deactivated)

**Fact:** RemoteDesktop's D-Bus input interface has no modifier query. Current
EI sends XKB modifier/group events to sender contexts, including changes from
other keyboards, but only for devices with a keymap. These are logical state
masks, not individual physical-key observations; raw key events belong to
receiver contexts. A sender sync orders its earlier EI requests and their direct
responses. **Inference:** it does not synchronize the separate shortcut D-Bus
stream or prove that all physical keys are released. [EI keyboard protocol](https://libinput.pages.freedesktop.org/libei/interfaces/ei_keyboard/index.html),
[RemoteDesktop API](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)

**Fact:** Mutter 50.0 sends initial seat modifier masks and subscribes to keymap
state changes; the inspected 49.0 adapter lacks this path. KDE's Plasma 6.6.90
changelog lists adding modifier feedback to EIS senders. **Proposed choice:**
represent modifier state as **unknown**, **down**, or **released**; validate usable
feedback per supported compositor/version. Pending initialization, pause, or
disconnect must invalidate usable state. Unknown must not become `false` or a
fixed-delay assumption. Offer explicit copying/manual paste when safety cannot
be established. [Mutter 50.0](https://raw.githubusercontent.com/GNOME/mutter/50.0/src/backends/meta-eis-client.c),
[Mutter 49.0](https://raw.githubusercontent.com/GNOME/mutter/49.0/src/backends/meta-eis-client.c),
[KDE changelog](https://kde.org/announcements/changelogs/plasma/6/6.6.5-6.6.90/)

**Fact:** InputCapture reroutes captured input to the application, activates at
compositor-controlled pointer barriers, and has no immediate capture request.
**Inference:** it cannot supply Speakeasy's passive background Escape contract.
[InputCapture API](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)

## Pill presentation

**Fact from the pinned local dependency:** GPUI 0.2.2's Wayland constructor
unconditionally creates `wl_surface`, `xdg_surface`, then `xdg_toplevel` before
returning. Wayland roles last for the surface's lifetime, including after the
role object is destroyed; layer-shell rejects a surface with another role.
**Conclusion:** a post-creation raw `configure_pill` cannot convert this GPUI
window to layer-shell. Native Wayland needs a pre-role construction seam or a
separately owned rendering surface. [GPUI 0.2.2 Wayland source](https://docs.rs/crate/gpui/0.2.2/source/src/platform/linux/wayland/window.rs),
[Wayland surface roles](https://wayland.freedesktop.org/docs/html/apa.html#protocol-spec-wl_surface),
[layer-shell protocol](https://raw.githubusercontent.com/swaywm/wlr-protocols/master/unstable/wlr-layer-shell-unstable-v1.xml)

**Fact:** the layer-shell library's support matrix includes Plasma, wlroots,
Smithay, and some Mir compositors, but excludes GNOME Wayland. **Inference:**
layer-shell alone cannot deliver broad desktop coverage, even after adding a
GPUI seam. [Layer-shell desktop support](https://github.com/wmww/gtk4-layer-shell#supported-desktops)

**Fact from GPUI 0.2.2:** X11 `WindowKind::PopUp` sets the EWMH notification type,
but its constructor does not set override-redirect or an empty input shape.
EWMH permits notification hints on override-redirect windows; ShapeInput governs
pointer hit testing. **Proposed prototype:** configure the owned X11 pill before
mapping with notification type, override-redirect, no keyboard focus/activation,
and an empty input shape. This retains GPUI and requires Xwayland on Wayland
hosts. **Gap:** these hints do not guarantee Mutter/KWin stacking, focus, or
fullscreen behavior. [GPUI 0.2.2 X11 source](https://docs.rs/crate/gpui/0.2.2/source/src/platform/linux/x11/window.rs),
[EWMH window types](https://specifications.freedesktop.org/wm/latest/ar01s05.html),
[X Shape protocol](https://xorg.freedesktop.org/archive/X11R7.7/doc/xextproto/shape.html)

## Tray and packaging

**Fact from the pinned dependency:** tray-icon 0.25.1 supports `ksni`, a
StatusNotifierItem D-Bus backend with its own worker and menu snapshots. It can
avoid GTK/AppIndicator/libxdo dependencies with default features disabled.
GNOME's AppIndicator extension adds this tray support. **Proposed choice:** reuse
`ksni`, detect absence of a tray host, and keep Settings/relaunch controls usable
without making an extension mandatory. [tray-icon source](https://github.com/tauri-apps/tray-icon#cargo-features),
[GNOME tray extension](https://extensions.gnome.org/extension/615/appindicator-support/)

**Fact:** pinned NeMo v0.1.0 documents glibc 2.31+, bundled CUDA user libraries
with a required compatible driver, and host Vulkan loader/driver use. An open
upstream report describes its Linux x86_64 CPU tarball crashing on AVX2 hardware
without AVX-512. **Proposed choice:** publish a declared app ABI baseline and
verify the engine's CPU baseline before promising a universal CPU fallback;
pin archives/checksums and test real fixture inference on baseline hardware.
Do not infer CPU compatibility from an archive name or successful model loading.
[NeMo v0.1.0 installation requirements](https://raw.githubusercontent.com/NVIDIA/NeMo-Speech.cpp/v0.1.0/docs/install.md),
[reported CPU artifact failure](https://github.com/NVIDIA/NeMo-Speech.cpp/issues/23)

## Verification gaps

No microphone, global input hook, clipboard mutation, input injection, or
downloaded engine execution was performed for this note. Linux release claims
need explicit native acceptance on genuine X11, GNOME Wayland, Plasma Wayland,
and the selected wlroots/Smithay environments. Check portal denial/revocation,
hold release order, sticky/layout modifiers, cancellation and disconnect cleanup,
clipboard owner changes, native Wayland and X11 editors, fullscreen stacking,
click-through, focus retention, mixed-DPI monitors, absent trays, and engine ABI
and CPU compatibility. Portable mocks cannot establish those desktop guarantees.

## Implementation appendix: cached bindings and owned resources

**Checked locally:** ashpd 0.13.13 and zbus 5.19.0 are cached; reis and libei
bindings are not. For ashpd, disable defaults and select `tokio`,
`global_shortcuts`, `remote_desktop`, `clipboard`, and `screencast`. The last
feature is needed because `remote_desktop` imports `screencast::Stream` without
enabling it automatically. This does not require requesting screen capture.
ashpd reexports zbus/zvariant. [ashpd manifest](https://docs.rs/crate/ashpd/0.13.13/source/Cargo.toml),
[RemoteDesktop source](https://docs.rs/crate/ashpd/0.13.13/source/src/desktop/remote_desktop.rs)

**Binding facts:** `GlobalShortcuts::create_session` returns
`Session<GlobalShortcuts>`; `bind_shortcuts` accepts `&[NewShortcut]` and returns
`Request<BindShortcuts>`. Its response exposes the bound shortcuts.
`receive_activated`, `receive_deactivated`, and `receive_shortcuts_changed`
return streams whose events expose `session_handle()` and action IDs. Subscribe
before binding and filter the owned session. [shortcut bindings](https://docs.rs/crate/ashpd/0.13.13/source/src/desktop/global_shortcuts.rs)

**Ownership limitation:** ashpd's request helper waits for `Response` before
returning `Request<T>`; `.response()` must then be called exactly once to check
success. A pending dialog therefore cannot be closed through the returned
request. `Session<T>` offers `close()` and `receive_closed()`, but has no Drop
cleanup, Clone, or public path accessor. **Proposed choice:** use cached zbus
directly for these narrow interfaces, retaining known request/session object
paths and subscribing before calls. Send `Request.Close` on cancellation and
`Session.Close` on shutdown; dropping an awaiting future is insufficient.
[request helper](https://docs.rs/crate/ashpd/0.13.13/source/src/proxy.rs),
[session wrapper](https://docs.rs/crate/ashpd/0.13.13/source/src/desktop/session.rs)

**Clipboard bindings:** `Clipboard::request` precedes RemoteDesktop start;
`SelectedDevices::is_clipboard_enabled()` checks the result. `set_selection`
advertises MIME types. `receive_selection_transfer` yields
`(Session<T>, String, u32)`; `selection_write(session, serial)` returns an owned
FD, followed by `selection_write_done(session, serial, success)` after writing
and closing. Owner-change events expose `session_is_owner: Option<bool>`.
The application must own the bytes, transfer tasks, cancellation, and session
identity. [clipboard bindings](https://docs.rs/crate/ashpd/0.13.13/source/src/desktop/clipboard.rs)

**Optional pure Rust route:** reis 0.7.1 supports
`ei::Context::new(UnixStream::from(portal_fd))`,
`handshake_tokio(name, ContextType::Sender)`, and a
`Stream<Item = Result<EiEvent, Error>>`. `EiEvent::KeyboardModifiers` contains
device, serial, depressed/latched/locked masks, and group. Devices expose their
keymap and interfaces; text capability and `ei::Text::utf8` exist. The crate
describes itself as incomplete and is not available in the current cache.
[reis Tokio source](https://docs.rs/crate/reis/0.7.1/source/src/tokio.rs),
[reis event source](https://docs.rs/crate/reis/0.7.1/source/src/event.rs),
[reis status](https://docs.rs/reis/0.7.1/reis/)

**Dynamic libei route:** the official C header provides the following symbols.
Load optional feedback/text symbols separately from basic sender support; keep
the library loaded until every owned object has been released.
[libei header](https://libinput.pages.freedesktop.org/libei/api/libei_8h_source.html)

| Operation | C declarations or values |
| --- | --- |
| Context | `struct ei *ei_new_sender(void *); struct ei *ei_unref(struct ei *);` |
| Portal FD and polling | `int ei_setup_backend_fd(struct ei *, int); int ei_get_fd(struct ei *); void ei_dispatch(struct ei *);` |
| Events | `struct ei_event *ei_get_event(struct ei *); struct ei_event *ei_event_unref(struct ei_event *);` |
| Modifier getters | `uint32_t ei_event_keyboard_get_xkb_mods_depressed(struct ei_event *);` plus `latched`, `locked`, and `ei_event_keyboard_get_xkb_group` |
| Constants | `EI_DEVICE_CAP_KEYBOARD = 1<<2`, `EI_DEVICE_CAP_TEXT = 1<<6`, `EI_EVENT_KEYBOARD_MODIFIERS = 9` |

`ei_setup_backend_fd` takes FD ownership. Poll its borrowed event FD, drain all
events, and unref even unknown event types. Retained device/seat pointers need
their own refs. `ei_seat_bind_capabilities` is variadic with a NULL terminator,
not a bitmask call. Bind offered capabilities once and wait for a resumed device.
UTF-8 sender functions and the text capability are documented since libei 1.6;
runtime symbols do not prove compositor support. [libei lifecycle](https://libinput.pages.freedesktop.org/libei/api/group__libei.html),
[seat binding](https://libinput.pages.freedesktop.org/libei/api/group__libei-seat.html),
[text sender API](https://libinput.pages.freedesktop.org/libei/api/group__libei-sender.html)

**Keymap facts:** the exact getter is
`struct ei_keymap *ei_device_keyboard_get_keymap(struct ei_device *)`.
`ei_keymap_get_fd` returns a borrowed `int`, `ei_keymap_get_size` a `size_t`, and
`ei_keymap_get_type` an enum (`EI_KEYMAP_TYPE_XKB = 1`). Parse that device's map;
look up modifier indices rather than treating every nonzero mask as held Ctrl,
Alt, Shift, or Super. Caps/Num Lock alone must not block insertion. Cached
xkbcommon-dl 0.4.2 supplies optional library loading and the needed keymap/state
functions. Missing feedback or parsing keeps state unknown. [device keymap](https://libinput.pages.freedesktop.org/libei/api/group__libei-device.html),
[keymap API](https://libinput.pages.freedesktop.org/libei/api/group__libei-keymap.html),
[XKB state interpretation](https://xkbcommon.org/doc/current/group__state.html)

## Pinned NeMo Linux assets

**Release metadata:** these are v0.1.0 Linux x86_64 archives, checked through
GitHub's API without downloading binaries. The CUDA asset is named `cuda`, not
`cuda12`; the latter suffix belongs to aarch64 assets. [release metadata](https://api.github.com/repos/NVIDIA/NeMo-Speech.cpp/releases/tags/v0.1.0)

| Filename | Bytes | SHA-256 |
| --- | ---: | --- |
| `nemo-speech-0.1.0-linux-x86_64-cpu.tar.gz` | 4,583,913 | `0f74131d631ad2c694cf0ec53490866bb6461147959589a69fb6fc231944065b` |
| `nemo-speech-0.1.0-linux-x86_64-cuda.tar.gz` | 107,310,946 | `e68628f396489c98fb353e070efaea5bc4977409ae7734fce56c251a79e29147` |
| `nemo-speech-0.1.0-linux-x86_64-vulkan.tar.gz` | 18,014,113 | `ce7b7c3c8771cb7450b26e6d4bd8fb2c5e35bcd9fe0076387f35052e9b9523ae` |

**Source contract:** the pinned installer accepts `bin/nemo-speech` at the archive
root or beneath one enclosing directory. CMake installs libraries under its
GNUInstallDirs library directory and sets Linux RPATH to `$ORIGIN` and the
relative sibling library directory. Preserve the whole runtime layout. The
exact tar listing and bundled CUDA major remain unverified; metadata alone
cannot establish them. [installer layout](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/v0.1.0/scripts/install.sh),
[installation/RPATH definitions](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/v0.1.0/CMakeLists.txt)


## Host application registration

**Fact:** unsandboxed apps associate a D-Bus connection with their installed
`.desktop` basename using `org.freedesktop.host.portal.Registry.Register`.
Registration must be the first portal call and occurs once per connection.
The candidate sends it directly before constructing other portal proxies; old
frontends without this interface retain their launcher-derived identity. The
launcher and GPUI window identity use `io.github.krvh.speakeasy`. This needs
acceptance with a launcher installed in the desktop's application search path.
[Registry contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.host.portal.Registry.html),
[GNOME developer explanation](https://blogs.gnome.org/ignapk/2025/06/04/using-portals-with-unsandboxed-apps/).
