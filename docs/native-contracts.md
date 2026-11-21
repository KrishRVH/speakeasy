# Native contracts

These contracts govern the platform adapters and the locally maintained GPUI adapters. Consult the
[change and test map](architecture.md#change-and-test-map) before changing their interfaces. The
Rust ownership types and checked boundaries enforce the local obligations described here; foreign
APIs also require these thread, buffer, and callback lifetime rules.

## Owned threads and insertion authority

Owners: `crates/platform/src/owned_thread.rs`, `input.rs`, `insertion.rs`, `insertion/commit.rs`,
`insertion/continuation.rs`, `clipboard.rs`, `monitor.rs`; `crates/app/src/show/recorder.rs`,
`show/wsl.rs`, and `instance.rs` hold the app's recorder, WSL lookup, and instance-listener threads.

`OwnedThread` owns exactly one join handle. Its exit lane closes after the body returns or unwinds;
thread-local destructors can still run afterward. Async join therefore performs the final OS join on
Tokio's blocking pool, while Drop always joins synchronously. The blocking closure owns the whole
thread owner, so cancelling a join cannot detach native cleanup. `OwnedThread::join` reports a
worker panic, a missing Tokio runtime, or a failed join task without exposing panic content. A
missing runtime still triggers the synchronous fallback. `StoppableThread` closes its stop lane
before joining. Stop requests do not acknowledge cleanup. Owners that depend on thread-local
destruction await the full join before opening replacements.

The Show recorder explicitly delivers its terminal outcome on its dedicated native thread. Its guard
delivers an unwind failure only if normal delivery has not started. Both paths hide the canvas and
respect session cancellation. The fallback may wait for the owner to drain the bounded event lane;
dropping that final event could leave the session waiting forever.

Insertion authority is a generation-tagged atomic commit gate shared with native cancellation
callbacks. An old permit cannot revoke or commit a newer recording. Final authorization checks occur
after blocking clipboard acquisition and immediately before an irreversible write/send; successful
commitment is not permission to repeat input after an error. Synthetic key arrays remain live for
their synchronous submission and carry the app's own-input marker so hooks ignore them. Direct text
is split only at valid encoding boundaries. Windows and macOS type each line break, whether CRLF, CR
or LF, as Shift+Enter, which chat and form fields take as a new line rather than a send; Linux sends
line breaks inside EI text, and the compositor decides how to insert them. Windows input cut short
releases every key its sent part left down. Clipboard data and decoded images never enter
Debug/error output.

`KeyCount` is a relaxed atomic counter. The Windows keyboard hook adds each key press it passes
through that is not a modifier or lock key. The Mac event tap adds each key-down it passes through,
since modifiers arrive as FlagsChanged, and adds one whenever the tap was disabled and may have
missed typing. Both callbacks skip keys with the own-input marker first. The insertion worker
compares the reading it takes after one paste with the one it takes before the next: the count and
the pointer position, and on macOS the HID system's mouse-down counters, which Speakeasy never posts
to. No other memory needs ordering against the count. A pointer that cannot be read means no
reading, and no space. On macOS the worker first asks the main thread for `secure_input` through the
main dispatch queue and waits 50 ms; secure input, or no answer, also means no reading.

A dictation sent with Return spends its permit on the text; the Return 100 ms later is a second send
that requires the foreground window (macOS: the frontmost app) captured right after the text,
Speakeasy not holding focus, and a released shortcut, so a newer press or a focus change withholds
it. A failure after the text landed reports that Return was not pressed, never a lost paste. A sent
dictation keeps its clipboard snapshot unused, so a slow target cannot paste the restored copy and
then receive Return. A paste snapshots the clipboard under the same lease that writes the dictation,
and the insertion thread answers the request before it waits 500 ms and puts the snapshot back. The
write records the sequence number or change count once its copy is complete, on Windows after the
clipboard closes, because closing advances the number. The restore reopens the clipboard, prepares
the whole snapshot, and writes only if that number is still unchanged. Cancellation before
submission restores the same snapshot immediately or clears the dictated copy when no snapshot was
available, as does a write that fails partway, under the number read once it has failed, or a
failure after the write; these restores are best effort, so the dictation still reports its
cancellation or the original error. Windows and macOS inspect every clipboard format or item type
for concealed content before reading any payload, and Windows saves nothing when its format listing
ends in an error; macOS checks that the pasteboard still holds the dictated text by reading the one
item whose types it has just inspected. The thread serves the next request only after the restore,
which therefore snapshots the restored copy. The snapshot lives only on the insertion thread and is
dropped after the restore.

`MonitorControl` serializes publishing a live run-loop handle, readiness and stop. Stop-before-start
remains effective; retirement removes the handle before a later stop can wake it. This lock cannot
become owner-only state because a blocked native run loop does not receive ordinary owner messages.

The WSL lookup thread is the one app thread nothing joins: its single lookup can block in the
Windows redirector for as long as WSL takes to start, or indefinitely on a wedged share. Handles ask
it over a channel, each question carrying its own one-slot reply lane, and a caller that stops
waiting drops that lane, so a late answer is discarded. The thread ends once every handle is gone
and the lookup has returned; process exit ends one still blocked.

The instance listener owns its current-thread Tokio executor and bounded set of control tasks for
receipt/disconnect waits. Shutdown aborts and drains those tasks before the server thread joins;
application state changes remain on the Services or session owner. The server thread builds its
executor and reports startup before `Instance::acquire` returns, so no executor is created or
dropped on the acquiring thread.

## Windows handles, hooks, windows and dialogs

Owners: `crates/platform/src/windows.rs`, `windows/input.rs`, `windows/insertion.rs`,
`windows/dialog.rs`, `windows/presentation.rs`, `windows/login.rs`, `windows/installation.rs`,
`windows/registry.rs`, `wsl.rs`; the test-only `windows/fixture.rs`.

`take_handle` accepts only a fresh unowned kernel-object handle from an API that reports failure
with null and requires `CloseHandle`; it is not an adopter for pseudo-handles, window handles, or
APIs using `INVALID_HANDLE_VALUE`. The OwnedHandle closes it exactly once. Native byte/UTF-16
buffers have checked ABI lengths and remain alive through the synchronous call. Registry strings are
bounded and terminated, then validated before becoming an installation path or a WSL share name: the
default distribution's key holds only hex digits, braces, and hyphens, and its name only letters,
digits, `.`, `_`, and `-`. Reading `/etc/wsl.conf` through `\\wsl.localhost` or `\\wsl$` can start
the WSL VM and block, so the app reads it only on the WSL lookup thread; the CLI reads it
synchronously. The file is bounded to 64 KiB; an unreadable or oversized file gives no WSL path.

Windows presentation/subclass operations run on the HWND's owning thread. `Window::new` verifies
that ownership. The pill and overlay reapply their physical bounds after a DPI transition because
GPUI can reposition them synchronously during the initial move. A subclass is removed during native
destruction. Low-level hook callbacks without a context argument use thread-local state only on
their owning hook/message-loop thread, and callback failures cannot unwind across the native ABI.
The input owner removes hooks/window registrations before releasing their reachable state. A raw
integer HWND transports identity, not ownership. The test-only `native_window` requires a live HWND
that outlives its borrowed handle. `keyboard_hook` casts `lparam` only for a nonnegative hook code,
when Windows supplies a `KBDLLHOOKSTRUCT` valid through that invocation; other codes are forwarded
unchanged. `Monitor` shares its `StopEvent` with the native thread so owner cancellation can wake a
blocked message wait; the final `Arc` releases the kernel event only after both users finish.

The file dialog owns a dedicated STA/message-loop thread and all title/filter/output buffers until
`GetOpenFileNameW` returns. Successful COM initialization is balanced on that same thread. Dropping
the selection future does not cancel or join this modal dialog; its result is discarded if the
receiver left. This intentional detached lifetime must remain explicit rather than being mistaken
for the joined worker contract.

`Awake` holds one power request from `PowerCreateRequest`, which copies its reason string, and sets
`PowerRequestSystemRequired` on it, plus `PowerRequestDisplayRequired` for a Show when the session
has a display; a service session without one refuses that part and keeps the system request. The
handle is a kernel object with no thread affinity, so the session owner may hold and drop it on any
runtime thread; dropping it clears each type it set and closes the handle. A refused request records
without it. On the Mac it is an `NSProcessInfo` activity that disables idle system sleep, and idle
display sleep for a Show; `NSProcessInfo` is thread-safe, so its token moves with the owner and ends
on drop. Linux holds nothing.

## Child process containment

Owner: `crates/platform/src/process.rs`.

Termination authority originates only from a freshly spawned child. Windows attaches its process
handle to a private kill-on-close job immediately after spawn; listener PID lookups only test
membership and never grant authority. Unix children must start with `process_group(0)`, and their
owner polls for exit with `waitid(WNOWAIT)` and terminates the group before reaping the leader. The
same check verifies that the leader is still this process's unreaped child; consuming the stored
leader before signaling prevents a second signal after PID reuse. Listener table parsing and `/proc`
inspection remain read-only membership evidence.

## Show directory handles

Owner: `crates/platform/src/directory.rs`.

A Show directory handle keeps every relative open anchored to the selected directory. Unix uses
`openat` with `O_NOFOLLOW` and `O_NONBLOCK` for child entries and validates the resulting descriptor
before reading. Linux opens directories with `O_PATH`, so a directory its owner may search but not
list still opens; macOS opens them for reading, which needs list permission. Windows opens a single
child name with `NtOpenFile` relative to its parent handle, refuses alternate data streams, and
transfers the handle to `File` only after a successful open. The opened entry's normalized long name
must equal the requested name apart from ASCII case, so an 8.3 short name cannot reach a hidden
entry. Name-surrogate reparse points (symbolic links, junctions and mount points) are neither files
nor directories, so both platforms refuse them by type; other reparse points, such as cloud
placeholders, open as their file or directory. Unix uses `fdopendir` on an independent directory
open, and Windows uses an empty relative name with `NtOpenFile` and `NtQueryDirectoryFile`, so
listing owns its cursor. Readers validate size, type, and bytes through the same handle. Identity
compares device and inode on Unix, and on Windows the volume's 128-bit file ID, or the 64-bit file
index where a file system has no 128-bit ID.

Windows handles share read and delete, so they never refuse a rename or delete of their own entry;
directory handles and ordinary reads also share write, so editing continues while a Show is read.
Movie reads exclude writers while a validated prefix is in use. NTFS refuses to rename or move any
directory above an open directory handle, so a root an MCP server holds for its session pins its
ancestors until the server exits.

`open_lock` gives Unix lease files mode 0600, refuses links and multiply linked files, and narrows
an older file's mode. Narrowing cannot revoke a descriptor another user opened before it, and macOS
ACL entries the file inherited from its folder still apply.

## Linux desktop and transfer ownership

Owners: `crates/platform/src/linux.rs`, `linux/ei.rs`, `linux/portal.rs`,
`linux/portal/insertion.rs`, `linux/portal/transfer.rs`, `linux/token.rs`, `linux/x11.rs`,
`linux/x11/clipboard.rs`, `linux/x11/paste.rs`.

`linux::watch_readable` gives readiness registration its own duplicated descriptor, owned for the
entire `AsyncFd` lifetime. Both the libei and X11 adapters use this registration.

The libei function pointers use the declared C ABI, including integer-zero termination of variadic
capability lists. `RTLD_NODELETE` keeps copied pointers valid after the library handle drops.
Context/device/event references are adopted and released once; device references drop before their
context. The EIS descriptor transfers to libei, while readiness and keymap access own duplicates.
The sender and native pointers remain on their desktop thread/LocalSet. Prepared input borrows the
sender exclusively from capability/modifier validation through submission, so queued dispatch cannot
replace the selected device during that interval. EIS modifier events govern paste readiness. A
keyboard starts neutral and returns to neutral when paused, so a resumed keyboard with a usable
layout can paste before its first modifier event; resuming preserves the last reported state. Global
shortcut signals can arrive out of order with those events and never reset the modifier state. A
disconnect ends the service even though later dispatch calls may succeed.

Portal transfer descriptors are owned, set nonblocking before AsyncFd registration and retained
until the transfer future drops. Portal signal match rules name the portal's unique bus name rather
than its well-known one, so zbus also drops a signal another client addresses straight to
Speakeasy's connection; a malformed shortcut or transfer signal is skipped, and a malformed
ownership change counts as lost ownership. Session-scoped signal streams keep draining while
transfers are full. A transfer Speakeasy declines, for a full transfer set, a selection still being
set, another MIME type, or no current committed copy, still gets SelectionWrite: its descriptor
closes empty and SelectionWriteDone reports failure, since Mutter ends a transfer only when its
descriptor closes. Refusals run beside the insertion loop, at most `SIGNAL_BACKLOG` at once; a
request past that bound waits for the portal's own timeout. The portal clipboard serves bytes only
after the permit commits to paste or manual copy. Selection loss invalidates its transfer payloads.
Writes and reporting have deadlines. X11 clipboard serving and insertion have separate owners; after
blocking focus/modifier queries and before committing paste, the X11 paste confirms that the
clipboard still holds the dictated text from the same owner. That check and XTEST submission are not
one atomic server transaction.

## Linux realtime opt-out

Owners: `crates/platform/src/linux/realtime.rs`, called first in `crates/app/src/main.rs`.

cpal's ALSA host opens the default device through PipeWire's ALSA plugin, which loads PipeWire's
[realtime module](https://docs.pipewire.org/page_module_rt.html) into Speakeasy's process. Without
realtime privileges, that module asks the desktop's Realtime portal or RTKit for a budget and lowers
`RLIMIT_RTTIME` to it, so a portal that cached a zero budget gets Speakeasy killed by the kernel
seconds after the microphone opens. Capture is buffered and needs no realtime scheduling, so
`disable_rtkit` sets
[`DISABLE_RTKIT`](https://docs.pipewire.org/page_man_pipewire_1.html#pipewire-env__disable_rtkit)
before any other thread starts. The module then never asks the portal or RTKit and fails to load,
which PipeWire's client configuration tolerates, and the stream opens without it. A user with
realtime privileges still gets them under the module's own limits.

Writing the environment races any thread that reads it, so `disable_rtkit` writes only while
`/proc/self/task` lists the calling thread alone, and otherwise leaves the environment unchanged.
Processes Speakeasy starts inherit the variable. Its engine, encoder, and archive helpers play no
audio, but an application Speakeasy opens through `xdg-open`, such as the player for a Show's movie,
also streams through PipeWire without realtime scheduling unless it holds realtime privileges.
PipeWire releases through 1.6 read the variable and its development branch ignores it, so a later
PipeWire release needs another opt-out.

## Mac event tap and AppKit ownership

Owners: `crates/platform/src/macos.rs`, `macos/clipboard.rs`, `macos/status_item.rs`, `monitor.rs`,
`permissions.rs`.

AppKit window/view operations require the main thread. The platform wrapper retains the NSWindow and
is not Send. Core Foundation Create-rule results are adopted once; Get-rule references remain
borrowed or are explicitly retained. Toll-free bridging casts are limited to APIs representing the
same underlying CF/Objective-C object, with the original owner retained through the borrow.

The event tap, its borrowed policy/context and run-loop source live on the same monitor thread. The
run-loop entry observer's stack context outlives the loop and is removed before returning. Entry
publication is necessary because stopping a not-yet-entered loop is not a reliable future stop. The
tap port is published once before events can run, allowing disabled-tap recovery. Lifecycle
callbacks can arrive on the posting thread; they touch only the synchronized policy, the input lane
and the monitor's stop request. Policy decisions and delivery occur under one lock to preserve event
order. Callback panics close the lane and stop the loop. The monitor thread registers a waker on the
input lane's closure, so a lane closed from any thread, including by its receiver, stops the run
loop without waiting for another event. The waker only stops the loop and takes no lock, so a lane
closed under the policy or monitor lock cannot deadlock. The event tap passes the current key
through and disables itself when the input lane closes. Notification tokens remain retained until
removal. A swallowed hold key stays suppressed until its own release or a fresh press of it, which
shows that the release went unseen, so a key held through a tap outage, lifecycle interrupt or
missed chord release never repeats into the app. The Mac tap tells a fresh press from a repeat by
the event's autorepeat field.

Mac F13 uses key code 105 and the event's autorepeat field. Its initial press and release establish
the pre-event hold explicitly; unrelated events reconcile against HID-system physical key state. The
secondary Fn flag can accompany function keys, so it does not make F13 another modifier chord. The
insertion worker retains the configured shortcut and polls physical F13 state for both its release
wait and its final preflight. Its per-request autorelease pool encloses native delivery.

Windows F13 is VK 0x7C, and the hook swallows its every event. Windows updates async key state only
after the hooks pass an event, so a swallowed F13 never reads as down, and its own events establish
the hold. The low-level hook reports no autorepeat, so the Windows policy takes a key-down as a
repeat only when the same key was the last one pressed and its previous press came at most 2.5 s
earlier, beyond the slowest repeat Windows offers; hold keys use the same test. A fresh F13 press
while F13 is held, or any event after F13 repeated and then fell silent past that limit while still
the last key pressed, shows that its release went unseen and ends the hold first; on a keyboard that
never repeats, only the fresh press does. A lifecycle interrupt stops its dictation as it stops
every chord's, and F13 repeats after it never restart one. F13 modifies no other key, so the
insertion worker's release wait and preflight do not wait for it.

`StatusItem` is created, updated and removed on the main thread. It takes the item's button at
creation, removing the item again if AppKit offers none, and holds that main-thread-only button, so
it is not Send. It owns the muda menu and retains muda's borrowed NSMenu pointer once, while that
menu is held, as the status item's own menu. AppKit therefore tracks the menu from the user's own
click, as for any status item with a menu, rather than from a synthesized `performClick`; muda still
dispatches item actions as `MenuEvent`s and updates the same NSMenu. Dropping the item removes it
from the status bar.

`secure_input` reads Carbon's `IsSecureEventInputEnabled`, which reports whether any process holds
Secure Event Input. The `unsafe extern` block that links Carbon vouches for its argumentless
`Boolean` signature. Its header marks it not thread safe, so the main thread is the precondition
each call discharges: `secure_input` calls it only once `MainThreadMarker` proves that thread, and
reports false elsewhere. While another process holds Secure Event Input, the session tap still
receives FlagsChanged but no KeyDown or KeyUp.

Permission queries report consent only; they do not open a microphone, prove device readiness or
install a hook. The Mac clipboard's private/transient marks precede user content, and change-count
plus content checks bound clipboard replacement races.

## Capture buffers, geometry and privacy

Owners: `crates/platform/src/screen.rs`, `screen/geometry.rs`, `windows/screen.rs`,
`windows/screen/exclusion.rs`, `windows/screen/layout.rs`, `windows/screen/readback.rs`,
`macos/screen.rs`, `macos/screen/stream.rs`, `macos/screen/stream/output.rs`; owned-window checks in
`windows/screen/acceptance.rs`, `windows/screen/acceptance/geometry.rs`, and
`windows/screen/acceptance/movie.rs`.

Windows capture runs on one owner thread with a free-threaded frame pool. Each capture takes an MTA
usage reference it never releases, so the apartment lasts for the process, because cached WinRT
activation factories outlive individual captures. Session/pool closure precedes resource release,
and the display-closed callback only closes its lane. Texture/context resources belong to the same
D3D device; crop bounds lie inside the source; CPU mappings are read only until Unmap. Checked
row/pitch/height calculations precede raw slices. App-owned windows, including any whose display
affinity cannot be read, are excluded before admissible frames, and new exclusions discard pending
images under the existing composition margin.

Pending Windows frames retain their observed target rectangle and are rejected when observations
before or after GPU readback differ. While DWM cannot report the frame bounds of a window that still
exists on screen, Windows discards its frames instead of ending the Show. Mac window stills are
admitted only when the target rectangle observed after the request equals that attached to the
image. A still rejected because the measurable target rectangle changed refreshes the response
clock; unknown geometry does not. Neither platform's sampled observations establish exact
composition-time window geometry or detect every move-away-and-back.

Mac `Source` may cross threads because its retained ScreenCaptureKit filter is immutable and lacks
thread affinity; no AppKit object is moved with it. The filter excludes the whole application,
including subsequently opened app windows. Fixed-region streams retain the native stream, output and
private serial queue until stop acknowledgement and queue drainage. A failed start detaches the
output and drains the queue directly. For a started stream, only successful stop, the exact
documented already-stopped domain/code, or the actual did-stop signal establishes cessation; other
stop errors do not. Each stop waits at most five seconds for its completion or the did-stop signal,
whichever comes first. An unconfirmed stop is reported and retried after 0.1, 0.4, 1 and 2 seconds.
If none confirms, capture reports that it may still be running and asks the user to quit, and the
stream, output and queue stay retained for the life of the process, since releasing them could leave
capture calling freed objects; the closed frame lane makes the output discard every sample. A stream
dropped before confirming cessation requests stop once more first. Failure notification precedes
potentially blocking cleanup. Callback samples are borrowed only during the call; copied bounded
BGRA leaves the callback. Native image buffers stay locked through reads. The stream's bounded
host-clock calibration and display-geometry checks precede frame admission.

A screen Show's window watch lists windows on its own stoppable thread every frame interval and
sends only changed layouts over a bounded lane, dropping a change the lane cannot take until the
next interval. It reads bounds, a layer class, and the owning process; never titles, app names, or
pixels. Windows marks topmost windows other than taskbars, menus, and popups floating; Mac marks
levels above normal and up to utility floating, the pop-up menu level a menu, and the main-menu and
Dock levels bars. Window and process identifiers become small per-watch numbers before they leave
the thread. Windows walks `EnumWindows` front to back and reads DWM extended frame bounds for
visible, uncloaked, not minimized windows that accept clicks; Mac reads on-screen
`CGWindowListCopyWindowInfo` entries. Both leave out Speakeasy's process, as frames do.

Owned-window acceptance creates and destroys its window classes and HWNDs on their owning thread.
Monitor enumeration writes only to the local vector that remains live throughout the synchronous
`EnumDisplayMonitors` call. The rescaling test window procedure casts `lparam` only for the
corresponding `WM_NCCALCSIZE` or `WM_DPICHANGED` message and uses its buffer only during that
callback.

## Shared movie presentation and checkpoint lease

Owners: `crates/platform/src/movie.rs`, `movie/canvas.rs`, `movie/encode.rs`, `movie/recording.rs`.

One recorder owns presentation scheduling, canvas, native codec and checkpoint lease. Geometry is
durably acknowledged before the first submitted sample using it. All grid points earlier than a
newly captured image use the previous canvas; quiet periods repeat already captured pixels. Windows
quantizes to 100 ns and Mac keeps nanoseconds while native samples retain their rational 30 Hz
timing. Per-advance catch-up, total sample, canvas and disk bounds remain independent. Coverage
records an accepted sample before a later checkpoint error can occur. A held image never claims a
new capture time.

Begin/geometry acknowledgements observe both checkpoint cancellation and capture stop. The
non-cloneable target drops after the native encoder and its actual file handles. A recording that
failed drops its codec without finalizing it. Finalization failure reports partial video; it cannot
erase committed coverage or unlock/delete checkpoint storage while callbacks retain file access.

## Windows codec and verified-file decoder

Owners: `crates/platform/src/windows/movie/foundation.rs`, `encoder.rs`, `convert.rs`, `file.rs`,
`read.rs`, `read/pixels.rs`.

The Foundation guard starts COM MTA and Media Foundation and is deliberately not Send. It outlives
every native codec/reader object on its initiating thread, including partial-open failures. Encoding
releases writer, sink, converter and stream before Foundation teardown. Conversion is synchronous,
disables frame-rate conversion and requires one output with unchanged time/duration for each input.
The generated MFT output's ManuallyDrop sample/events are owned by a guard and released exactly once
on every result path.

The COM file adapter serializes cursor operations and caps actual writes, seeks and SetSize. Foreign
pointers and optional result pointers obey IStream's caller-owned buffer contract. It maps failures
to content-free HRESULTs. Reads, reported size and end-relative seeks stay within the validated
prefix even if the underlying file grows. Explicit IMFByteStream Close cancels I/O, but the Closed
receipt waits for the last underlying IStream/file owner. A writer's extra File handle exists only
to sync after Finalize closes the native byte stream, and drops before that receipt.

Decoding uses the same validated open File, never reopens a path. Media-type-change notifications
revalidate dimensions/color format and associate stride with the returned sample. Flat and 2D native
locks remain owned until unlock; signed pitch, offsets and allocation bounds are checked before
copying top-down opaque BGRA. Cancellation is observed between native calls, and native I/O is
released before return.

## Mac codec and verified-file decoder

Owners: `crates/platform/src/macos/movie.rs`, `macos/movie/file.rs`, `macos/movie/encode.rs`,
`encode/output.rs`, `encode/buffers.rs`, `encode/settings.rs`.

AVAssetImageGenerator requests one exact rational timestamp with no preferred transform or aperture
crop. The request retains the generator and waits for the promised completion even after
cancellation or unwind. Completion borrows native objects only during its call and copies checked
pixels/timing into Rust-owned values. Numeric native error codes may be reported; native
descriptions and userInfo are not exposed.

The asset's private resource URL resolves only to the validated file. Its strongly held delegate
backs a weak native registration; external references are forbidden. Each bounded read holds the
file mutex only through file access, releasing it before responding with copied NSData. Asset
teardown detaches/cancels and takes the optional File under the same mutex, so no concurrent read
retains the file afterward. The reader passes the published movie bytes directly to AVFoundation;
Media Foundation fragment headers are normalized before publication.

The AVAssetWriter has no output URL: only the strongly held segment delegate owns its create-new
private file. Segment order, actual byte cap and sync are enforced under one lock. Failure/panic is
latched before releasing that lock. Each appended sample owns an independent CVPixelBuffer; the pool
allocation threshold is based on actual buffer size. Writable pixel slices are exclusive and bounded
by the native allocation while locked. CMSampleBuffer/format results follow Create ownership and
retain their dependencies. Finalization requires a writing writer and waits for its native
completion. A writer that fails asynchronously between that status check and the finalization call
still makes AVFoundation raise an Objective-C exception, which aborts the process; catching it needs
objc2's `exception` feature, whose helper crate the build does not include. Drop cancels a writer
still writing before closing the sink. The checkpoint lease outlives those resources.

## GPUI and vendored adapters

Owners: the [vendor index](../vendor/README.md) lists each patched vendor file;
`crates/app/src/application.rs`, `main.rs`, `shell/shutdown.rs`, and
`shell/settings/text_field/accessible.rs` hold the app side.

GPUI's `AsyncApp::update` panics if the app is gone or already borrowed. Every `AsyncApp` Speakeasy
uses comes from `cx.spawn`, and foreground tasks run only from the platform loop while
`Application::run` holds the app. `App::shutdown` blocks on its quit futures alone, so no foreground
task runs under its borrow. Speakeasy never calls `to_async` or keeps an `AsyncApp` beyond its task.

`App::shutdown` runs the quit observers, then disposes windows and flushes effects, then polls the
observers' futures for up to `SHUTDOWN_TIMEOUT` (200 ms). Speakeasy's observer removes the services
before any window is disposed, and Settings finishes its queued saves as its window is disposed. The
forced-exit fallback keeps the instance lock inside the returned future until window disposal has
completed. Settings stays owned by its window alone.

On macOS a deferred termination (`NSTerminateLater`) runs AppKit's nested run loop, which cannot
drain the serial main queue that GPUI's foreground tasks need. The Apple dispatcher marks main-queue
tasks with a flag only the main thread writes, and `applicationShouldTerminate:` defers only outside
such a task and outside an application borrow. The platform takes its state lock only around reads
and writes of the termination state, never across the app's handler or AppKit calls.

Vendor patches follow upstream documentation policy. Windows UIA native event raising and provider
return occur outside window-state borrows because either can synchronously reenter through
`WM_GETOBJECT`. Hide publishes an empty root-only tree before the window disappears and minimize
publishes it as the window minimizes; the tree returns only after `WM_WINDOWPOSCHANGED` commits
visibility. `WM_DESTROY` publishes the empty tree while the HWND is valid, then returns a null
provider for the HWND so UI Automation drops its entries for the window. A provider retires when its
node leaves the tree, even when a later tree reuses the id, because GPUI derives node ids from
element ids, and a text range retires with the provider that created it. Range actions are requested
after the tree lock is released and report errors instead of panicking across COM. Bounds are
physical client coordinates, and clipping must not turn disjoint or inverted rectangles into visible
content. A range scroll request carries the range rather than coordinates, so the field resolves it
against the layout it paints next. GPUI queues action requests and handles them on the foreground
after the adapter returns, so a request made just before a hide reaches the restored tree when its
element survives; rejecting it needs a tree generation in GPUI's action path. The macOS and Linux
AccessKit bridges carry no Speakeasy patches and do not withdraw a hidden window's tree.
