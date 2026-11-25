# Architecture

The session machine in `speakeasy-core` decides what happens. The app owns its state and routes
effects to audio, speech, Show recording, and native insertion. GPUI presents snapshots. The
platform crate owns native access, and the Show crate composes recordings without I/O. See the
[crate map](../AGENTS.md#ownership-and-interfaces) and [native contracts](native-contracts.md).

## Change and test map

Tests live with their modules. Use the linked integration tests when a change crosses an owner.

- **Gestures and the five-minute limit.** [Gesture](../crates/core/src/gesture.rs) converts timed
  input into actions. Show promotion keeps the original clock. Check
  [Gesture sequences](../crates/core/src/gesture/tests.rs) and
  [owner dictation](../crates/app/src/runtime/tests/dictation.rs).
- **Session decisions.** [Machine](../crates/core/src/machine.rs) takes one event at a supplied time
  and reaches side effects through `Env`. [Session](../crates/core/src/machine/session.rs) owns each
  session's stage and permit. Check [Machine tests](../crates/core/src/machine/tests.rs) and
  [exhaustive exploration](#exhaustive-exploration).
- **Runtime and devices.** [Owner](../crates/app/src/runtime/owner.rs) interprets the machine
  through [ports](../crates/app/src/runtime/ports.rs). [Device](../crates/app/src/runtime/device.rs)
  and [worker](../crates/app/src/runtime/worker.rs) hold active and retiring work. Check
  [Owner fakes](../crates/app/src/runtime/tests.rs), including
  [recognition](../crates/app/src/runtime/tests/recognition.rs),
  [streaming](../crates/app/src/runtime/tests/streaming.rs), and
  [replay](../crates/app/src/runtime/tests/replay.rs).
- **Microphone and cues.** [Audio](../crates/app/src/audio.rs) seals a trimmed WAV before teardown.
  [Speech gate](../crates/app/src/audio/speech.rs) measures energy around the window mean and
  discounts the app's own cues. [Sounds](../crates/app/src/sounds.rs) owns output. A capture that
  loses its microphone after speech keeps the usable audio. Check
  [Synthetic audio](../crates/app/src/audio/tests.rs),
  [stream](../crates/app/src/audio/stream/tests.rs), and
  [cue routing](../crates/app/src/runtime/tests/cues.rs).
- **Local inference.** [Local speech](../crates/app/src/local_speech.rs) starts children and talks
  over loopback. [Streaming](../crates/app/src/local_speech/streaming.rs) accepts final Nemotron
  events. [Models](../crates/app/src/models.rs) adapts language and
  [recognition terms](../crates/app/src/recognition_terms.rs) supplies per-request hints. Check
  [Fake engine](../crates/app/src/local_speech/tests.rs) and the opt-in
  [public replay](performance.md#public-corpus-replay).
- **Transcript fidelity.** [Transcript](../crates/app/src/transcript.rs) applies filler cleanup and
  corrections before symbols and style. Corrections preserve timed spans. Show narration has its own
  policy. Check [transcript modules](../crates/app/src/transcript) and
  [owner recognition](../crates/app/src/runtime/tests/recognition.rs).
- **Input and delivery.** [Shortcut](../crates/platform/src/shortcut.rs) names, parses, and checks
  the keys a person chooses. [Keyboard](../crates/platform/src/keyboard.rs) shares Windows and Mac
  key policy and the shortcut recorder. [Desktop](../crates/platform/src/desktop.rs) supplies each
  adapter. [Insertion](../crates/platform/src/insertion.rs) commits once, with platform-specific
  focus and clipboard handling. Check [key policy](../crates/platform/src/keyboard/tests.rs),
  [shortcuts](../crates/platform/src/shortcut/tests.rs), and
  [commit tests](../crates/platform/src/insertion/commit/tests.rs). Linux setup is in
  [Linux implementation](linux-implementation.md).
- **Presentation.** [Pill](../crates/app/src/pill.rs) animates snapshots with
  [motion](../crates/core/src/motion.rs). [Tray](../crates/app/src/tray.rs) and
  [status](../crates/app/src/status.rs) present service and session state.
  [GPUI extensions](../crates/app/src/gpui_ext.rs) defer native show and hide calls that can reenter
  drawing. Check [Pill tests](../crates/app/src/pill/tests.rs) and the opt-in `native:rendering`
  task.
- **Settings and presets.** [Settings](../crates/app/src/shell/settings.rs) owns the draft.
  [Save queue](../crates/app/src/shell/save.rs) holds one write and the newest queued edit.
  [Config](../crates/app/src/config.rs) merges changed values with current disk settings and
  replaces the file by rename. [Choices](../crates/app/src/shell/choices.rs) passes validated
  presets to Services and the owner. Check [Config](../crates/app/src/config/tests.rs),
  [save queue](../crates/app/src/shell/save/tests.rs), and
  [control](../crates/app/src/runtime/tests/control.rs).
- **Settings UI.** [Text field](../crates/app/src/shell/settings/text_field.rs) owns the draft,
  selection, IME, and accessible edits. IME offsets are UTF-16 relative to marked text.
  [Accessible ranges](../crates/app/src/shell/settings/text_field/accessible.rs) follow the painted
  viewport and retire on edits. [Onboarding](../crates/app/src/shell/settings/onboarding.rs) keeps
  completion beside the selected settings. Check `native:settings`, `windows:accessibility`, and
  [practice](../crates/app/src/runtime/tests/practice.rs).
- **Service lifecycle.** [Lifecycle](../crates/core/src/lifecycle.rs) drives
  [Services](../crates/app/src/shell/services.rs) through Pause, Resume, reconfiguration, and
  [Quit](../crates/app/src/shell/shutdown.rs). [Reset](../crates/app/src/reset.rs) removes downloads
  before removing settings and preserves Shows. Check
  [Lifecycle](../crates/core/src/lifecycle/tests.rs), [reset](../crates/app/src/reset/tests.rs), and
  [Mac quit acceptance](../scripts/check-update-macos.sh).
- **Installation and updates.** [Setup](../crates/app/src/setup.rs) resumes downloads and verifies
  the pinned SHA-256 before naming a completed download. [Updates](../crates/app/src/updates.rs)
  discovers releases. [Install](../crates/app/src/updates/install.rs) verifies Ed25519 metadata and
  the installer's size and SHA-256. Installation follows the Quit drain and instance release. Check
  [Preparation](../crates/app/src/updates/install/tests.rs) and
  [installer acceptance](../scripts/check-installer.ps1).
- **Relaunch and scripts.** [Instance](../crates/app/src/instance.rs) locks per settings folder and
  serves one JSON request per loopback connection. [Control](../crates/app/src/control.rs) parses
  commands. [Owner commands](../crates/app/src/runtime/owner/commands.rs) authorize live calls and
  bind stop or cancel tokens to ordinary dictation sessions. Check
  [Console integration](../crates/app/tests/cli.rs),
  [control](../crates/app/src/runtime/tests/control.rs), and
  [script printing](../crates/app/src/runtime/tests/printing.rs).
- **Show capture.** [Recorder](../crates/app/src/show/recorder.rs) starts after an explicit Show
  request. [Screen](../crates/platform/src/screen.rs) supplies geometry with samples.
  [Overlay](../crates/app/src/show/overlay.rs) draws live ink. Observation gaps break incomplete
  actions. Check [Recorder fixtures](../crates/app/src/show/recorder/tests.rs),
  [owner Shows](../crates/app/src/runtime/tests/shows.rs), and the opt-in capture tasks.
- **Show composition.** [Show crate](../crates/show/src) retains and thins samples, tracks marks,
  selects and crops keyframes, aligns narration, and renders sheets.
  [Locate](../crates/show/src/locate.rs) resolves time, marks, and phrases. Report actual sampled
  times. Check [Crop corpus](../crates/show/src/framing/tests/corpus.rs),
  [alignment](../crates/show/src/align/tests.rs), and
  [source queries](../crates/app/src/show/command/source/tests.rs).
- **Show storage and recovery.** [Store](../crates/app/src/show/store.rs) owns paths and
  publication. [Checkpoint](../crates/app/src/show/store/checkpoint.rs) owns live logs and locks.
  [Recall](../crates/app/src/show/store/recall.rs) supplies one unreadable error per Show.
  [Retention](../crates/app/src/show/store/retention.rs) owns Keep and expiry. Check
  [Store](../crates/app/src/show/store/tests.rs),
  [checkpoint](../crates/app/src/show/store/checkpoint/tests.rs), and
  [publication recovery](../crates/app/src/show/publish/tests/recovery.rs).
- **Show review and export.** [Review](../crates/app/src/show/review.rs) owns selection and read
  receipts. [Preview](../crates/app/src/show/preview.rs) retains the reader through cleanup.
  [Edits](../crates/app/src/show/edits.rs) owns accepted writes. CLI and review share
  [ZIP](../crates/app/src/show/store/export.rs) and [video](../crates/app/src/show/store/video.rs)
  exporters. [Copy](../crates/app/src/show/copy.rs) waits for native cleanup before admitting
  another copy. Check [Review](../crates/app/src/show/review/tests.rs),
  [copy](../crates/app/src/show/copy/tests.rs), and `native:review`.
- **Show format and agent queries.** [Bundle](../crates/show/src/bundle.rs) and
  [reader](../crates/show/src/bundle/read.rs) own the current format.
  [Command](../crates/app/src/show/command.rs) and [MCP](../crates/app/src/show/mcp.rs) share
  [evidence reads](../crates/app/src/show/evidence.rs). MCP resolves Show folders within explicit
  roots. The [bundled skill](../skills/speakeasy-show/SKILL.md) also supplies MCP's reading guide.
  Check [Bundle](../crates/show/src/bundle/tests.rs),
  [commands](../crates/app/src/show/command/tests.rs), and
  [MCP](../crates/app/src/show/mcp/tests.rs).
- **Show video.** [Movie recorder](../crates/platform/src/movie/recording.rs) owns the native
  encoder and its checkpoint lease. [Movie prefix](../crates/show/src/movie.rs) finds complete
  fragments. [Store movie](../crates/app/src/show/store/movie.rs) publishes the playable prefix and
  geometry. [Video annotations](../crates/show/src/draw/video.rs) use the decoded sample's timing
  and geometry. Check [Encoder scheduling](../crates/platform/src/movie/encode/tests.rs) and the
  opt-in `movie:` tasks listed in [Build and verify](../README.md#build-and-verify).
- **Packaging and patches.** [Scripts](../scripts) use resources in [packaging](../packaging) and
  generate notices from the locked graph. [Vendor index](../vendor/README.md) owns the patch
  contracts. Check [Python script tests](../tests),
  [package smoke check](../scripts/check-package.sh), and
  [portable adapter tests](../crates/platform/tests).

## Cross-module invariants

- **Revoke before cleanup.** A dictation's permit is revoked synchronously on cancellation, a
  settings change the session reads, Pause, or Quit. Native insertion checks it just before
  committing. A Show shields the permit from input cancellation so the machine can decide whether to
  keep or discard the recording. A hands-free recording shields it too, since bare Escape belongs to
  the focused app, and exposes it again to deliver. Pause, Quit, and reconfiguration still revoke
  publication.
- **Match results to the session.** Capture, footage, and recognition results carry identity.
  Retired work stays owned until cleanup and cannot change a newer recording. A Show that already
  committed can still become the last Show unless a newer one has been saved.
- **Retire before reopening.** Await actual native cleanup before replacing microphone, screen,
  worker, insertion, or desktop resources. Use owned state and channels for effects. Snapshots may
  coalesce and the meter travels separately. Neither drives session decisions.
- **Treat the recording as authoritative.** Parakeet pieces and Nemotron streaming are optional
  early work. Keep the full audio for fallback. Seal before sending text. Apply corrections to the
  combined transcript. Show promotion uses whole-recording narration with timing.
- **Keep delivery explicit.** A failed or partial native submission never triggers an automatic
  second insertion. Paste last uses the exact previous result with a fresh permit. Microphone loss
  after speech can populate that slot without submitting text. Practice and script prints have their
  own destinations. A print requires an opted-in capture and the matching token.
- **Serialize settings and lifecycle.** Services sends configuration changes to the owner before
  subsequent input. Land the latest save, or the last successful write if a newer save fails. Keep
  edits made during a save. Stage rollback and its restart until idle. Hide leaves the Settings view
  and its save queue alive. Application-owned Quit drains accepted writes, devices, and background
  work before releasing the instance. Forced GPUI teardown is covered in
  [native contracts](native-contracts.md).
- **Publish with one commit.** Stage the Show in a temporary folder beside the library and rename it
  into place after the permit commits. A failed clipboard copy leaves the saved bundle. Keep and
  seven-day expiry apply to the whole bundle, including saved voice. Accepted edits remain tied to
  the Show the user chose.
- **Retain finished checkpoints through cancellation.** The session chooses checkpoint disposition.
  Finish waits for both log producers and the native movie lease. Checkpoint failure can leave
  footage publishable from memory. Quit, Pause, and reconfiguration preserve a finished Show that
  has yet to publish. Recovery reads surviving records, uses the original language with current
  corrections, and publishes a new bundle without touching the microphone or clipboard. It preserves
  the original expiry. Pending narration is a separate checkpoint with that original expiry, even if
  the earlier bundle is kept.
- **Keep timing and geometry together.** A source query returns a retained sample at or before the
  requested time, or the first if it precedes capture. Its time and any gap still matter. Video
  carries a separate annotation horizon. Draw and crop against the actual decoded sample's geometry.
  Preserve completed video when its tail is interrupted.

### Destruction order

`Capture.signal`, `Sounds.asks`, and `ScreenRecorder.finish` close before their threads join. Audio
streams drop before host and device handles. `LocalSpeech.group` terminates the child before it is
reaped. The instance removes its port file before releasing its lock. Settings requests cancellation
before joining setup and updates. `Owners` requests both runtime and monitor stop before waiting for
either.

## Local speech

Use [the shared HTTP builders](../crates/app/src/http.rs). Linux needs the shared rustls provider
and platform certificate verification for internet requests. The engine's health route gates
startup. A successful silent inference, used for warmup or worker reuse, establishes that it can run
an inference and that abandoned work has drained. Constructing a worker-load future is inert until
the owner polls it after retirement. Keep the local engines' macOS Metal tensor-kernel and Windows
OBS Vulkan-layer opt-outs and the Linux CPU inference probe.

Whisper timed requests split segments at word boundaries. Keep the first word of each segment
separate and join continuation tokens within that word. The owner filters isolated Whisper credits
and repeated prompt text on brief audio using measured audible time. Nemotron's stream only delivers
final results, and a failed stream falls back to the same model.

## Exhaustive exploration

The [explorer](../crates/core/src/machine/explore.rs) runs the pure session machine over admitted
event orders and fake answers, checking
[invariants](../crates/core/src/machine/explore/invariants.rs) at each transition. The
[canonical key](../crates/core/src/machine/explore/canonical.rs) merges equivalent sessions and
timings. When changing an event or a query, update the explored event alphabet and oracle where the
owner could take that branch. Default tests search to depth five. `mise run rust:explore` runs to a
fixed point within the explorer's scaled timing and retirement bounds. Failures print their event
path.

## Verification

Run `mise run standards:check` for the host workspace and native adapter cross-checks. App changes
behind Windows or macOS cfgs need their own target checks. The
[native task list](../README.md#build-and-verify) names tasks that open windows, use synthetic movie
fixtures, or request screen capture. Filter any manual ignored-test run to the intended test.
Default tests use fakes or public fixtures, including private socket servers for engine, instance,
and portal tests. The [performance guide](performance.md) covers public audio replay and live
measurements.
