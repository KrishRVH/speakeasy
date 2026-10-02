# Apple-silicon performance research

API research date: 2026-09-30; source mapping updated 2026-10-02 UTC for released Speakeasy 0.3.2 at
`cd302b4c864ea3ea64decd47d8fa9eee721c6088`. This note supports the
[fork experiment PRD](apple-silicon-prd.md). It separates documented API capabilities, frozen source
facts, and optimization hypotheses. The pinned engine/CPAL API findings are retained; this update
adds no native performance evidence. It does not establish that a rewritten Mac app is faster than
GPUI: this workspace is Linux/WSL2, with no native Mac, Swift toolchain, Xcode, Instruments, or
Apple GPU available for this investigation. No microphone, global event tap, clipboard, or focused
application was accessed.

## Recommendation and evidence boundary

Collect the frozen 0.3.2 native baseline, then select prototypes from measured costs: a Swift/AppKit
shell around the existing Rust owner and a native PCM helper alongside the HTTP worker. Retain the
same Parakeet/Metal model/runtime as the quality and speed control. Keep capture and transport
behind narrow existing side-effect boundaries. A direct HAL implementation, Core ML conversion,
custom Metal kernels, and alternative buffer layouts are candidates to earn their place with
measurements.

Performance decisions must consider microphone onset, stop-to-submission tail latency, cancellation,
displayed feedback, idle energy, and memory pressure together. A warm model, speculative inference,
very small audio periods, and fully committed maximum-length buffers can improve one metric while
worsening others. Neither a language choice nor a framework name proves a win. These are engineering
decisions, not results from a cross-stack benchmark. The PRD owns the blinded perceptibility
protocol and valid no-gain outcomes; resource or microbenchmark gains alone cannot answer whether
the app feels different.

The [0.3.2 source map](apple-silicon-baseline.md#source-audit-and-experiments) and
[architecture](architecture.md#change-and-test-map) identify reusable owner, lifecycle, keyboard,
insertion, and regression-test modules. The session code has no direct GPUI imports but still needs
library extraction from the app crate; preserve its typed stages, publisher epochs, paused clock,
and retirement ordering. The 0.3.2 control also has typed capture cancellation, checked WAV/rate and
gesture deadline boundaries, bounded instance requests with shutdown wakeup, and owned threads,
monitors, and children that are joined, woken, or reaped before replacement. Carry those contracts
and their tests into extracted modules; the native shell must retain verified reopen compatibility
and incomplete-insertion recovery.

## Scope, versions, and deployment floor

The existing application promises macOS 14+ on Apple silicon and already uses
`MACOSX_DEPLOYMENT_TARGET=14.0`. Preserve that floor unless an experiment shows why a higher minimum
is necessary. Use native `aarch64-apple-darwin` builds; Rosetta is outside this target. Existing
behavior and current measurement limits are documented in [README](../README.md),
[architecture](architecture.md), and [performance](performance.md).

| Capability                                                     | Documented availability / implication                                                                                                                                                                       |
| -------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| AppKit nonactivating panel, Core Animation, CGEvent tap, AUHAL | Established APIs available at the current floor; no new OS floor needed merely to use native UI/capture.                                                                                                    |
| View-associated display link                                   | `NSView.displayLink(target:selector:)` and macOS `CADisplayLink` are macOS 14+. The view-associated link follows its display and does not invoke its callback while the view is hidden or off all displays. |
| Audio workgroups                                               | macOS 11+. System audio threads are already joined automatically; normal non-real-time consumer/inference threads must not be joined as though they were audio render threads.                              |
| Core ML ML Programs                                            | macOS 12+. The operations and model features selected during conversion may require newer deployment targets.                                                                                               |
| Core ML CPU + Neural Engine configuration                      | macOS 13+. This allows CPU and Neural Engine execution; it does not force all operations onto Neural Engine.                                                                                                |
| `MLComputePlan`, `MLModelStructure`                            | macOS 14.4+. Gate an optional inspection tool appropriately; these APIs need not raise the main app's floor.                                                                                                |
| `OSSignposter`                                                 | macOS 12+. Fixed-name intervals and numeric identities can instrument the app without logging audio or text.                                                                                                |

Sources:
[AppKit display link](<https://developer.apple.com/documentation/appkit/nsview/displaylink(target:selector:)>),
[CADisplayLink](https://developer.apple.com/documentation/quartzcore/cadisplaylink),
[audio workgroups](https://developer.apple.com/documentation/audiotoolbox/understanding-audio-workgroups),
[ML Programs](https://apple.github.io/coremltools/docs-guides/source/convert-to-ml-program.html),
[compute units](https://developer.apple.com/documentation/coreml/mlcomputeunits/cpuandneuralengine),
[compute plan](https://developer.apple.com/documentation/coreml/mlcomputeplan-1w21n),
[model structure](https://developer.apple.com/documentation/coreml/mlmodelstructure-swift.enum),
[signposter](https://developer.apple.com/documentation/os/ossignposter).

The engine source examined is Speakeasy's pinned NeMo-Speech.cpp `v0.1.0`, commit
`4f9676226f667d14608487df744f375db87127f8`, rather than assuming that current upstream `main`
describes the deployed binary. The app downloads Parakeet v3 Q8_0 from a pinned model revision;
retain those identities in paired experiments. See [setup source](../crates/app/src/setup.rs) and
the
[pinned engine tree](https://github.com/NVIDIA/NeMo-Speech.cpp/tree/4f9676226f667d14608487df744f375db87127f8).

The pinned release publishes `nemo-speech-0.1.0-macos-aarch64-metal.tar.gz` and an aarch64 CPU
archive. Its build enables `GGML_METAL` and installs `ggml-metal`. This confirms an actual native
Metal/arm64 delivery path, not merely a proposed backend. The examined release/build documentation
does not establish the minimum OS of every shipped Mach-O dependency. On a Mac inspect
architectures, `LC_BUILD_VERSION`/minimum-OS commands, runtime search paths and required symbols for
the helper and each library, then execute on the minimum supported OS. Rebuilding with an explicit
deployment target is preferable to inferring compatibility from the app's Info.plist. Sources:
[pinned release](https://github.com/NVIDIA/NeMo-Speech.cpp/releases/tag/v0.1.0),
[build configuration](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/4f9676226f667d14608487df744f375db87127f8/CMakeLists.txt).

## AppKit and the pill

AppKit provides the required primitives directly: an `NSPanel` with `.nonactivatingPanel` avoids
activating its app; `ignoresMouseEvents` supports click-through behavior. Nonactivation alone is not
the same contract as never becoming key. Explicitly prevent key/main status for this display-only
panel, and test actual foreground application/focus while showing it. Construct the panel with its
intended style rather than relying on late style mutation. Validate Spaces, full-screen auxiliary
presentation, multiple screens, display removal, backing-scale changes, and sleep/wake. Sources:
[nonactivating panel](https://developer.apple.com/documentation/appkit/nswindow/stylemask-swift.struct/nonactivatingpanel),
[key-window eligibility](https://developer.apple.com/documentation/appkit/nswindow/canbecomekey),
[mouse transparency](https://developer.apple.com/documentation/appkit/nswindow/ignoresmouseevents).

Candidate rendering design: retain static pill artwork in layers; animate transform/opacity and
update only changing meter content. Use a view-associated display link only while custom frame work
remains necessary, then pause or invalidate it. Preserve the existing interruption behavior,
analytic motion, reduced-motion preference, countdown, appearance, and all four themes. Settings can
be built lazily with native controls, but its unsaved draft and save coalescing remain
session-independent requirements.

Core Animation is not a guaranteed performance improvement. Apple's guide specifically calls for
measurement, appropriate redraw policy, avoiding needless backing-store redraws, and reducing
expensive shadows/complex drawing. Benchmark application CPU and wakeups **and**
WindowServer/compositor/GPU work; moving cost outside the app's PID is not eliminating it. Compare
equal visuals at 60 Hz and high refresh rates, including interrupted transitions and recording under
inference load.
[Core Animation performance guidance](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/CoreAnimation_guide/ImprovingAnimationPerformance/ImprovingAnimationPerformance.html).

Choosing Swift for AppKit is an integration decision. It avoids rebuilding UI bindings by hand but
is not evidence of lower latency than Rust invoking the same APIs. The real-time callback and large
buffers should remain outside reference-heavy UI objects and actor hops. Avoid per-sample
language-boundary calls; exchange status snapshots and bounded meter batches.

## Shortcut monitoring, permission, and secure input

The current Mac monitor installs an active session event tap on its owned run loop. Fn transitions
use key code 63 and the secondary-Fn flag; Escape is passed through. Fn+Space is deliberately
swallowed, including its matching release. Therefore changing the entire tap to `.listenOnly` would
violate parity even though passive Escape itself must remain passive. Keep exact hold, double-tap,
hands-free, interruption, and own-injected-event behavior.
[Current Mac adapter](../crates/platform/src/macos.rs),
[portable keyboard policy](../crates/platform/src/keyboard.rs),
[Fn flag semantics](https://developer.apple.com/documentation/coregraphics/cgeventflags/masksecondaryfn),
[passive tap option](https://developer.apple.com/documentation/coregraphics/cgeventtapoptions/listenonly).

`CGEventTapCreate` invokes the callback on the run loop to which its source is attached and may
return null or omit events that the process cannot monitor. Keep the callback bounded; invalidate
the insertion gate immediately before publishing cancellation. Tap timeout or user-disabled
notifications must retire the affected monitor/session rather than silently leaving a held gesture
active. Permissions are an actual runtime capability, not a property proved by a signed build.
Exercise Accessibility and event listen/post access independently in native acceptance; Apple's
modern preflight APIs exist at this deployment floor. Sources:
[event tap creation](<https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:)>),
[listen preflight](<https://developer.apple.com/documentation/coregraphics/cgpreflightlisteneventaccess()>),
[post preflight](<https://developer.apple.com/documentation/coregraphics/cgpreflightposteventaccess()>).

Secure Event Input can prevent keyboard intercept processes from receiving keyboard events,
including when the process enabling it is in the background. Apple provides
`IsSecureEventInputEnabled` to query this state. Treat this as an OS boundary; do not bypass it or
promise that passive Escape is always observable in protected input. Investigate safe transition
detection, fail-closed session retirement, modifier reconciliation, and actionable recovery without
persistent high-frequency idle polling. Fn/Globe behavior also requires native tests on built-in and
external keyboards and with the OS Globe action set to Do Nothing.
[Apple Secure Event Input technical note](https://developer.apple.com/library/archive/technotes/tn2150/_index.html)
is archived documentation; confirm current behavior on supported OS versions.

Insertion must retain clipboard ownership/change-count checks, the final cancellation commit, and
direct Unicode injection for Keep clipboard. A successful event post is submission, not proof an
arbitrary editor accepted text. Do not replace this with an Accessibility text setter by default:
application support and edit semantics are separate compatibility questions.
[Existing insertion contract](../crates/platform/src/macos.rs).

The [shared preflight](../crates/platform/src/insertion.rs) gives external-target focus precedence
over held modifiers. Keep cancellation checks and the single generation-bound commit in the native
adapter; a partial post-commit failure is never automatically repeated.

## Capture: AUHAL is already beneath CPAL

CPAL 0.18.2, the app's selected version, already uses a HAL Output AudioUnit with input enabled and
a native input callback on macOS. Its backend supports device selection, callback timestamps, and
fixed buffer periods within the reported hardware range. A new direct-AUHAL adapter would give
application-specific control; it would not replace an emulated or non-native capture path.
[CPAL 0.18.2 Mac device source](https://github.com/RustAudio/cpal/blob/v0.18.2/src/host/coreaudio/macos/device.rs).

Apple's AUHAL guidance explains that the unit handles simple PCM layout/format conversion but the
client's sample rate should match the device. Sample-rate conversion can occur on a separate thread.
Smaller periods reduce buffering but increase wakeup/processing frequency; changing a device's
format or sample rate can be disruptive. Do not change the user's device clock simply to simplify
ASR.
[Apple AUHAL capture note](https://developer.apple.com/library/archive/technotes/tn2091/_index.html).

The existing capture callback already publishes bounded mono float32 packets without scratch
allocation. The consumer wakes every five milliseconds while recording, computes levels, and
converts float32 to PCM16 bytes. Finish/cancel unparks it immediately. These facts identify
experiments, not measured Mac bottlenecks: replace polling with a real-time-safe notification
strategy; retain native timestamps; compare PCM16 staging with native float32; batch consumer work;
measure startup stages and actual first-sample capture. Preserve overflow, discontinuity,
device-loss, sample-count and wall-clock limit checks. [Audio source](../crates/app/src/audio.rs).

AVAudioEngine is another native candidate, but an `installTap` buffer size is a request and the
implementation may choose another size. It is not an equivalent guarantee of tiny callback periods.
Measure its onset, callback sizes, allocations, and teardown against CPAL/direct HAL if it is
proposed; do not adopt or reject it solely because it is higher level.
[AVAudioNode tap contract](<https://developer.apple.com/documentation/avfaudio/avaudionode/installtap(onbus:buffersize:format:block:)>).

The system already joins the audio framework's real-time thread to its device workgroup. Apple says
an app needs no additional work if it uses only that provided real-time thread. Auxiliary workgroup
membership applies to additional real-time audio threads with coordinated deadlines, not an ASR
worker. Keep allocation, locks, logging, UI, inference, and IPC serialization off the audio
callback. Investigate notification costs and missed-wakeup races under load; “event-driven” does not
prove a notification operation is real-time safe.
[Workgroup ownership](https://developer.apple.com/documentation/audiotoolbox/understanding-audio-workgroups).

Do not keep the device open, start a silent input stream, or retain a preroll microphone to win
onset benchmarks. Only inert buffers/metadata may be prepared outside recording; device-resource
preparation must be checked against the existing privacy contract. A five-minute 48 kHz mono float32
recording is 57.6 MB before overhead, versus 28.8 MB for PCM16; 96 kHz doubles both. These are
calculated payload sizes, not measured RSS. Preallocation, page commitment, resizing, and zeroing
must be measured separately under low-memory conditions.

## Native PCM and worker lifecycle

The pinned engine already has a stable C ABI with opaque recognizer/result handles and explicit
destruction. `nemo_speech_asr_recognize_f32` accepts mono float32 PCM and internally resamples 8–96
kHz inputs. A tiny helper can link only the ASR component. It can be implemented in Rust with a
platform FFI adapter or in C++ with RAII; the neural runtime remains the same C++/ggml
implementation. C++ for the helper wrapper is not inherently a speed requirement.
[Pinned SDK documentation](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/4f9676226f667d14608487df744f375db87127f8/docs/sdk.md),
[pinned ASR C header](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/4f9676226f667d14608487df744f375db87127f8/include/nemo_speech/asr.h).

The current app uses PCM16 in a fixed WAV header, multipart HTTP, and a JSON response. WAV here is
not a compressed codec: deleting its 44-byte header is not a meaningful optimization by itself. A
native PCM path can remove server parsing, transport staging, conversion and resampling duplication
where present. Measure all copies/allocations and full stop-to-result time to determine the gain; do
not present removal of HTTP as a large inference speedup in advance.
[Current speech worker](../crates/app/src/local_speech.rs).

The native offline ABI has no caller cancellation callback/token. The presence of a CANCELLED status
enum is not an interrupt contract. Do not destroy a recognizer concurrently with a running
recognition call. Maintain immediate session invalidation in the app, then use a documented
cooperative engine change or an independently owned process termination path for unresponsive work.
Keep model recovery and exit completion separate from visible cancellation.
[Pinned ASR implementation](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/4f9676226f667d14608487df744f375db87127f8/src/asr/c_api.cpp).

Evaluate these process/transport combinations independently:

| Candidate                                               | Benefit to investigate                                                            | Ownership cost / required experiment                                                                                                                                                |
| ------------------------------------------------------- | --------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| App-owned helper + inherited Unix socket                | Explicit spawn/termination/wait; no network listener; easy PCM protocol baseline. | Prove parent-death detection and helper exit while inference is blocked, close all unused inherited endpoints, and prevent old connections/results from acting on a new generation. |
| App-owned helper + shared-memory PCM and control socket | Avoid large payload socket copies.                                                | Measure mapping/allocation/page faults, buffer handoff and retained capacity; separate control lane must remain responsive during inference.                                        |
| Embedded XPC service + XPC messages/shared memory       | Native launch/authentication/IPC mechanism and crash isolation.                   | `launchd`, not the app, owns service creation/idle exit. Prove warm retention, abnormal exit, pause memory release, and bounded replacement without assuming waitpid ownership.     |
| In-process native SDK                                   | Removes process transport entirely.                                               | A native hang/crash affects the app; safe bounded cancellation and model cleanup must be solved first. Not the default recommendation.                                              |

XPC services can be started on demand, shut down while idle, and restarted after crashes.
Transactions can express ongoing activity; a service with no outstanding transactions may exit. A
deliberate warm-model transaction is a lifecycle policy to prototype, not evidence that a permanent
warm service is naturally owned by the app. Connection invalidation is not proof memory was released
or the old process/GPU work has exited. Sources:
[XPC lifecycle](https://developer.apple.com/documentation/xpc),
[transaction accounting](<https://developer.apple.com/documentation/xpc/xpc_transaction_begin()>).

`xpc_connection_cancel` is asynchronous and non-preemptive: an executing event handler finishes. It
must not be used as “cancel inference.” `xpc_shmem_create` only boxes an already shareable region
created with `mmap(..., MAP_SHARED, ...)`; it cannot make an ordinary malloc/Rust Vec allocation
shareable. The mapping needs a bounded length, validated offsets/format/sample count, immutable
ownership while inference reads it, and a release/acknowledgement protocol. No logging payloads or
named persistent audio files. Sources:
[XPC cancellation](<https://developer.apple.com/documentation/xpc/xpc_connection_cancel(_:)>),
[XPC shared memory](<https://developer.apple.com/documentation/xpc/xpc_shmem_create(_:_:)>).

Engine and setup children are killed when their owner drops and reaped before replacement. The
existing Unix process group terminates children on normal owner cleanup but has no OS
kill-on-parent-close equivalent: force-killing the parent bypasses Rust destructors. Native
crash/force-quit acceptance must prove that no orphan model remains. An app-owned helper can watch a
parent-owned connection on an independent control thread and exit on EOF; prove this during real GPU
inference and loss of the parent. External custom Whisper/NeMo executable support needs its own
containment policy rather than assuming the controlled helper protocol.
[Current process containment](../crates/platform/src/process.rs),
[owned children](../crates/app/src/child.rs).

The 0.3.2 [worker state](../crates/app/src/runtime/worker.rs) separates loading, transcribing,
recovery, and replacement. Its [Quit coordinator](../crates/app/src/shell/shutdown.rs) awaits owned
work and requested saves while the UI remains responsive. These normal-cleanup guarantees do not
establish containment after abrupt parent death.

## Metal and inference optimization

Apple GPUs use unified system memory, but Metal resource access and synchronization still depend on
storage mode. Use shared resources for CPU/GPU data; private resources can suit GPU-only
intermediates. Private does not mean a separate physical VRAM chip on Apple silicon. CPU writes must
finish before GPU reads, and buffers cannot be reused until the GPU has completed its access. Shared
PCM between two processes is also not automatically a Metal buffer in the engine. Sources:
[Apple GPU storage choices](https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus),
[CPU/GPU synchronization](https://developer.apple.com/documentation/metal/synchronizing-cpu-and-gpu-work).

Profile feature extraction, encoder graphs, decoder iterations, CPU/GPU transfers, command
submissions, synchronization/readback, allocation growth and thread counts separately. Then
investigate operator fusion, tensor layout, fewer intermediate allocations, command submission
batching, and model-state reuse. Preserve the same preprocessing, weights, tokenizer, decode policy
and quality checks for equivalent-path comparisons. More CPU threads, GPU use, private buffers or
lower precision are not unconditional improvements for short single-user input. These are hypotheses
derived from the runtime structure, not diagnosed Mac bottlenecks. The pinned runtime's Metal build
is supported on Apple silicon. The `metal-asr` preset also enables diarization; explicitly disable
unused components and verify the resulting linked/runtime dependency set. Do not assume the preset
name means a minimal ASR-only binary.
[Pinned build guide](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/4f9676226f667d14608487df744f375db87127f8/docs/build.md).

Parakeet v3 is a multilingual FastConformer-TDT model. Keep all existing supported languages,
punctuation/capitalization, and silence behavior. NVIDIA's quality tables exclude
punctuation/capitalization errors, so WER alone is insufficient for dictation parity. The model
card's long-audio figures describe specific attention configurations/hardware and are not a memory
or speed guarantee for an 8 GB Mac.
[NVIDIA model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3).

Pinned NeMo-Speech.cpp explicitly rejects Parakeet TDT cache-aware streaming; its native path is
full-utterance recognition. NVIDIA separately provides a buffered/chunked NeMo streaming example.
Moving work earlier through overlapping windows is therefore a different algorithm/runtime
experiment, with boundary accuracy, language identification, punctuation, extra GPU work and energy
costs. It cannot be specified as a free feature-parity latency win.
[Pinned model limitations](https://github.com/NVIDIA/NeMo-Speech.cpp/blob/4f9676226f667d14608487df744f375db87127f8/docs/asr/models.md),
[NVIDIA chunked-inference example](https://github.com/NVIDIA/NeMo/blob/main/examples/asr/asr_chunked_inference/rnnt/speech_to_text_streaming_infer_rnnt.py).

## Core ML / Neural Engine is an experiment

Core ML conversion starts from supported source graphs such as PyTorch, TensorFlow, or MIL. There is
no documented direct conversion from this engine's Q8_0 GGUF to equivalent Core ML execution. Begin
with the pinned original model checkpoint and matching preprocessing/tokenizer, then prove encoder
and decoder parity. An FP16 Core ML model is not bitwise equivalent to the current Q8_0 model.
Sources:
[conversion formats](https://apple.github.io/coremltools/docs-guides/source/target-conversion-formats.html),
[ML Program precision](https://apple.github.io/coremltools/docs-guides/source/convert-to-ml-program.html).

Variable utterance length, dynamic reshapes/control flow, attention and iterative TDT decoding are
conversion/partition questions. Apple recommends enumerated input shapes for compilation
optimization, but dynamic layers can prevent Neural Engine execution; padding/shape buckets need
output-length masks and quality checks. Composite supported operations are preferable to custom
layers; custom layers are not supported in ML Programs. Successful conversion alone does not
establish useful Neural Engine placement or lower latency.
[Flexible shape guidance](https://apple.github.io/coremltools/docs-guides/source/flexible-inputs.html),
[Neural Engine flexible-shape limits](https://apple.github.io/coremltools/docs-guides/source/faqs.html),
[custom operator limits](https://apple.github.io/coremltools/docs-guides/source/custom-operators.html).

Xcode's model performance report measures prediction, load and compilation and shows operation
placement. Core ML may choose CPU for an operation compatible with the Neural Engine because data
movement/ramp-up changes whole-pipeline cost. Measure CPU-only, CPU/GPU, CPU/Neural Engine and
all-allowed configurations using actual audio, including encoder/decoder boundaries, first use of
each shape, cached/uncached model load, OS-update invalidation and helper restart. The report does
not measure memory or power; it does not replace end-to-end fixture tests.
[Core ML model profiling](https://developer.apple.com/documentation/coreml/analyzing-a-core-ml-model-s-performance-in-xcode).

## Compilation and distribution

The control pins its compiler and tooling through `rust-toolchain.toml` and mise, and sets Rust thin
LTO, one codegen unit, and release overflow checks. Extracted Rust crates inherit workspace lints;
unsafe stays confined to the native adapter. Use `mise run standards:check` and the same pinned
control build when comparing candidates. Keep a symbolized release profiling artifact and compare
thin/fat/no LTO where measurements justify it; do not remove correctness checks merely to improve a
microbenchmark. Rust PGO supports an instrument/run/merge/rebuild workflow with representative
profiles and matching compiler flags. Generate profiles on the actual target workload, cover
error/cancel/recovery paths, and compare against an uninstrumented release. Do not assume
Rust/C++/Swift share one cross-language LTO or PGO domain.
[Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html),
[Rust PGO](https://doc.rust-lang.org/rustc/profile-guided-optimization.html).

Swift whole-module optimization enables cross-file specialization and inlining; its benefit depends
on the code. Verify current Release settings rather than assuming Debug or a historical Xcode
default is suitable. Clang/Swift/native framework build choices should be recorded independently
from Rust. [Swift optimization explanation](https://www.swift.org/blog/whole-module-optimizations/).

The existing Mac script uses an ad-hoc development signature. A distributable bundle needs Developer
ID signatures, hardened runtime, secure timestamps, notarization and the correct
organization/signing of nested helpers/libraries. Sign nested code explicitly in dependency order;
Apple's guidance warns against using `codesign --deep` as the signing strategy. Use strict nested
verification as a separate step. App Sandbox is optional for outside-App-Store distribution; do not
assume it is required or compatible with all existing custom-engine and global-input behavior.
[Notarization requirements](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution),
[distribution signing](https://developer.apple.com/documentation/security/creating-distribution-signed-code-for-macos),
[distribution preparation](https://developer.apple.com/documentation/xcode/preparing-your-app-for-distribution).

The recording process needs the microphone usage declaration and applicable audio-input entitlement
for hardened runtime. Keep a stable app identity and validate permission granting/revocation,
moving/relaunching the app, signed helper behavior, offline first launch after setup, and custom
executable launch under the packaged runtime. Notarization is distribution validation, not proof of
input/audio feature correctness.
[Microphone usage declaration](https://developer.apple.com/documentation/bundleresources/information-property-list/nsmicrophoneusagedescription),
[audio-input entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.device.audio-input).

## Native measurement plan and remaining questions

Use release builds and fixed public/licensed fixtures. Record hardware, memory, OS, toolchain,
engine/model hashes, power source/mode, thermal state, microphone format/route, refresh rate and
warm/cold state. Compare paired randomized baseline and candidate runs; report median/tail latency
and full distributions. Synthetic capture isolates app overhead, while opt-in native acceptance
measures actual microphone and insertion. Keep those claims distinct, and use the PRD's blinded
native tasks to assess perceptibility independently of engineering gains. Test a low-memory M1-class
machine, a recent Air-class laptop, and a Pro/Max-class system; Apple-silicon-only still contains
large compute, bandwidth and thermal differences.

| Tool / evidence                                                     | What it answers                                                                                                                                                                          |
| ------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fixed-name signposts + monotonic stage timestamps                   | Shortcut-to-open, first callback, first retained sample, stop/drain, request handoff, ASR stages, result delivery, insertion submission and worker exit. Numeric metadata only.          |
| Time Profiler, System Trace, Allocations/VM tools                   | CPU hot paths, polling/wakeups, main-thread stalls, allocation growth/page faults, app/helper footprint; retain matching symbols.                                                        |
| Audio System Trace                                                  | Callback timing, jitter, discontinuities and deadline overloads, correlated with system/thermal load.                                                                                    |
| Metal System Trace, GPU counters, Xcode GPU capture                 | Submissions, stalls, synchronization and GPU work, including inference/UI contention; instrument the helper explicitly.                                                                  |
| Core ML report / compute plan                                       | Supported/preferred operation placement and model load/prediction/compile timing; compute-plan costs are estimates.                                                                      |
| Activity Monitor energy impact / controlled `powermetrics` sampling | Relative energy diagnostics and hardware-dependent whole-system estimates; subtract matched idle and include helper/compositor cost. These are not universal per-app joule measurements. |

Sources:
[Audio System Trace](https://developer.apple.com/documentation/audiotoolbox/analyzing-audio-performance-with-instruments),
[Metal workload capture](https://developer.apple.com/documentation/xcode/capturing-a-metal-workload-in-xcode),
[Mac energy monitoring](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/MonitoringEnergyUsage.html).
The previously suggested
[Power Profiler article](https://developer.apple.com/documentation/xcode/measuring-your-app-s-power-use-with-power-profiler)
documents iPhone/iPad availability, not a Mac measurement workflow. Do not cite it as evidence that
this Mac utility has a supported per-app power instrument.

The first decisions needing native evidence are:

1. Is onset dominated by device start, first packet scheduling, ring consumption, or UI publication?
   Does a direct HAL adapter beat the existing CPAL HAL path?
2. How much latency and memory does native PCM transport actually remove on short and five-minute
   inputs? Are shareable mappings worth their setup cost?
3. Which helper ownership scheme preserves warm readiness and proves model release after Pause,
   cancellation recovery, crash and parent force quit?
4. Does a retained-layer AppKit pill reduce total system work with equal visual and interaction
   quality, including high-refresh displays and GPU inference?
5. Where does pinned Parakeet/Metal spend time, and what is its maximum-length memory behavior on
   low-memory Macs? Does Core ML improve the complete pipeline without language, punctuation,
   silence or word-loss regressions?
6. Which settings trade latency against energy/memory? Choose and document the measured objective
   before adopting background work, preload or precision changes.

The PRD should fix privacy, session safety, feature parity and measurable performance objectives. It
should leave unmeasured backend/transport/kernel choices as experiments with acceptance evidence,
rather than converting the preceding stack suggestions into mandatory implementation facts.
