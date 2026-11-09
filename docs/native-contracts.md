# Native contracts

These contracts govern the platform adapters and the locally maintained GPUI adapters. Consult the
[change and test map](architecture.md#change-and-test-map) before changing their interfaces. The
Rust ownership types and checked boundaries enforce the local obligations described here; foreign
APIs also require these thread, buffer, and callback lifetime rules.

## Owned threads and insertion authority

Owners: `crates/platform/src/owned_thread.rs`, `input.rs`, `insertion.rs`, `insertion/commit.rs`,
`clipboard.rs`, `monitor.rs`; `crates/app/src/show/recorder.rs` and `instance.rs` hold the app's
recorder and instance-listener threads.

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
is split only at valid encoding boundaries. Clipboard data and decoded images never enter
Debug/error output.

`MonitorControl` serializes publishing a live run-loop handle, readiness and stop. Stop-before-start
remains effective; retirement removes the handle before a later stop can wake it. This lock cannot
become owner-only state because a blocked native run loop does not receive ordinary owner messages.

The instance listener owns its current-thread Tokio executor and bounded set of control tasks for
receipt/disconnect waits. Shutdown aborts and drains those tasks before the server thread joins;
application state changes remain on the Services or session owner.

## Windows handles, hooks, windows and dialogs

Owners: `crates/platform/src/windows.rs`, `windows/input.rs`, `windows/insertion.rs`,
`windows/dialog.rs`, `windows/presentation.rs`, `windows/login.rs`, `windows/installation.rs`.

`take_handle` accepts only a fresh unowned kernel-object handle from an API that reports failure
with null and requires `CloseHandle`; it is not an adopter for pseudo-handles, window handles, or
APIs using `INVALID_HANDLE_VALUE`. The OwnedHandle closes it exactly once. Native byte/UTF-16
buffers have checked ABI lengths and remain alive through the synchronous call. Registry strings are
bounded, terminated and validated before becoming an installation path.

Windows presentation/subclass operations run on the HWND's owning thread. `Window::new` verifies
that ownership. A subclass is removed during native destruction. Low-level hook callbacks without a
context argument use thread-local state only on their owning hook/message-loop thread, and callback
failures cannot unwind across the native ABI. The input owner removes hooks/window registrations
before releasing their reachable state. A raw integer HWND transports identity, not ownership. The
test-only `native_window` requires a live HWND that outlives its borrowed handle. `keyboard_hook`
casts `lparam` only for a nonnegative hook code, when Windows supplies a `KBDLLHOOKSTRUCT` valid
through that invocation; other codes are forwarded unchanged. `Monitor` shares its `StopEvent` with
the native thread so owner cancellation can wake a blocked message wait; the final `Arc` releases
the kernel event only after both users finish.

The file dialog owns a dedicated STA/message-loop thread and all title/filter/output buffers until
`GetOpenFileNameW` returns. Successful COM initialization is balanced on that same thread. Dropping
the selection future does not cancel or join this modal dialog; its result is discarded if the
receiver left. This intentional detached lifetime must remain explicit rather than being mistaken
for the joined worker contract.

## Child process containment

Owner: `crates/platform/src/process.rs`.

Termination authority originates only from a freshly spawned child. Windows attaches its process
handle to a private kill-on-close job immediately after spawn; listener PID lookups only test
membership and never grant authority. Unix children must start with `process_group(0)`, and their
owner terminates the group before reaping the leader. `waitid(WNOWAIT)` verifies that the leader is
still this process's unreaped child; consuming the stored leader before signaling prevents a second
signal after PID reuse. Listener table parsing and `/proc` inspection remain read-only membership
evidence.

## Linux desktop and transfer ownership

Owners: `crates/platform/src/linux.rs`, `linux/ei.rs`, `linux/portal.rs`,
`linux/portal/insertion.rs`, `linux/portal/transfer.rs`, `linux/x11/clipboard.rs`,
`linux/x11/paste.rs`.

`linux::watch_readable` gives readiness registration its own duplicated descriptor, owned for the
entire `AsyncFd` lifetime. Both the libei and X11 adapters use this registration.

The libei function pointers use the declared C ABI, including integer-zero termination of variadic
capability lists. `RTLD_NODELETE` keeps copied pointers valid after the library handle drops.
Context/device/event references are adopted and released once; device references drop before their
context. The EIS descriptor transfers to libei, while readiness and keymap access own duplicates.
The sender and native pointers remain on their desktop thread/LocalSet. Prepared input borrows the
sender exclusively from capability/modifier validation through submission, so queued dispatch cannot
replace the selected device during that interval. A disconnect ends the service even though later
dispatch calls may succeed.

Portal transfer descriptors are owned, set nonblocking before AsyncFd registration and retained
until the transfer future drops. At most the bounded transfer set remains active. Selection loss
invalidates its transfer payloads; paste commitment alone does not, because an editor may request
bytes afterward. Writes and reporting have deadlines. X11 clipboard serving and insertion have
separate owners; selection ownership is rechecked after blocking focus/modifier queries and before
committing paste. That query and XTEST submission are not one atomic server transaction.

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
callbacks can arrive on the posting thread; they touch only the synchronized policy and input lane.
Policy decisions and delivery occur under one lock to preserve event order. Callback panics close
the lane/stop the loop instead of crossing the ABI. Notification tokens remain retained until
removal.

Mac F13 uses key code 105 and the event's autorepeat field. Its initial press and release establish
the pre-event hold explicitly; unrelated events reconcile against HID-system physical key state. The
secondary Fn flag can accompany function keys, so it does not make F13 another modifier chord. The
insertion worker retains the configured shortcut and polls physical F13 state for both its release
wait and its final preflight. Its per-request autorelease pool encloses native delivery.

`StatusItem` is created, updated and removed on the main thread and holds its `MainThreadMarker`, so
it is not Send. It owns the muda menu and retains muda's borrowed NSMenu pointer once, while that
menu is held, as the status item's own menu. AppKit therefore tracks the menu from the user's own
click, as for any status item with a menu, rather than from a synthesized `performClick`; muda still
dispatches item actions as `MenuEvent`s and updates the same NSMenu. Dropping the item removes it
from the status bar.

Permission queries report consent only; they do not open a microphone, prove device readiness or
install a hook. The Mac clipboard's private/transient marks precede user content, and change-count
plus content checks bound clipboard replacement races.

## Capture buffers, geometry and privacy

Owners: `crates/platform/src/screen.rs`, `screen/geometry.rs`, `windows/screen.rs`,
`windows/screen/exclusion.rs`, `windows/screen/readback.rs`, `macos/screen.rs`,
`macos/screen/stream.rs`, `macos/screen/stream/output.rs`; owned-window checks in
`windows/screen/acceptance.rs` and `windows/screen/acceptance/geometry.rs`.

Windows capture runs on one owner thread with a free-threaded frame pool. The process-lifetime MTA
usage reference is intentional because cached WinRT activation factories outlive individual
captures. Session/pool closure precedes resource release, and the display-closed callback only
closes its lane. Texture/context resources belong to the same D3D device; crop bounds lie inside the
source; CPU mappings are read only until Unmap. Checked row/pitch/height calculations precede raw
slices. App-owned windows are excluded before admissible frames and new exclusions discard pending
images under the existing composition margin.

Pending Windows frames retain their observed target rectangle and are rejected when observations
before or after GPU readback differ. Mac window stills are admitted only when the target rectangle
observed after the request equals that attached to the image. A still rejected because the
measurable target rectangle changed refreshes the response clock; unknown geometry does not. Neither
platform's sampled observations establish exact composition-time window geometry or detect every
move-away-and-back.

Mac `Source` may cross threads because its retained ScreenCaptureKit filter is immutable and lacks
thread affinity; no AppKit object is moved with it. The filter excludes the whole application,
including subsequently opened app windows. Fixed-region streams retain the native stream, output and
private serial queue until stop acknowledgement and queue drainage. Only successful stop, the exact
documented already-stopped domain/code, or the actual did-stop signal establishes cessation; other
stop errors do not. Failure notification precedes potentially blocking cleanup. Callback samples are
borrowed only during the call; copied bounded BGRA leaves the callback. Native image buffers stay
locked through reads. The stream's bounded host-clock calibration and display-geometry checks
precede frame admission.

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
non-cloneable target drops after the native encoder and its actual file handles. Finalization
failure reports partial video; it cannot erase committed coverage or unlock/delete checkpoint
storage while callbacks retain file access.

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
retain their dependencies. Finish waits for its native completion; unfinished Drop cancels before
closing the sink. The checkpoint lease outlives those resources.

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
