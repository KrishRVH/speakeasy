# Speakeasy's engine build

`scripts/build-engine.sh` builds NeMo-Speech.cpp v0.1.0 (`4f96762`) with ggml `c03b4e2` and
SentencePiece `17d7580` for Apple silicon on macOS 14 or later, with CPU code for the M1 baseline
(`armv8.5-a+fp16+dotprod`), and stages it under a name that hashes every input. These patches keep
the model, its weights, and its arithmetic; each graph change produced identical transcripts on
every [performance](../../docs/performance.md#engine-build) fixture.

`nemo-speech.patch` changes the offline Parakeet path. Each change keeps the original graph behind
an environment variable for paired comparisons in one build:

| Change                                                                                                               | Restore the original                   |
| -------------------------------------------------------------------------------------------------------------------- | -------------------------------------- |
| Channel-last subsampling: one matrix product per 1×1 convolution and strided multiply-adds per depthwise convolution | `NEMO_SPEECH_LEGACY_SUBSAMPLING=1`     |
| Conformer convolution module in [C, T] with BatchNorm folded into depthwise taps at load and one `SSM_CONV` kernel   | `NEMO_SPEECH_LEGACY_CONV=1`            |
| Transformer-XL relative shift as one strided view instead of a concat and two full-matrix copies per layer           | `NEMO_SPEECH_LEGACY_RELSHIFT=1`        |
| Attention products read head views directly; only V is copied for its transpose                                      | `NEMO_SPEECH_LEGACY_ATTN_COPIES=1`     |
| The TDT greedy decoder runs on a CPU backend beside a GPU encoder instead of one GPU round trip per step             | `NEMO_SPEECH_GPU_DECODER=1`            |
| CPU backend workers persist across graphs instead of being created per decoder step                                  | `NEMO_SPEECH_DISPOSABLE_CPU_THREADS=1` |

`NEMO_SPEECH_CPU_THREADS` overrides the CPU backend's four threads. `ggml.patch` embeds a compiled
Metal library beside the source, so loading the engine skips the runtime Metal compiler on devices
without the tensor API; `GGML_METAL_COMPILE_SOURCE=1` compiles the source instead.
