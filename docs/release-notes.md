# Release notes

Speakeasy 0.3.1 tightens Rust ownership and failure handling, adds a reproducible tooling gate, and
makes the current implementation easier to navigate and verify.

- Enforce documented public interfaces, checked integer arithmetic, explicit fallible-result
  handling, and locally justified synchronization. Keep unsafe FFI in the native adapters and
  preserve the bounded, allocation-free capture callback.
- Validate WAV lengths and sample rates, reject invalid process-group identities, and check setup
  staging failures with actionable errors. Regression tests cover these boundaries and bounded
  relaunch commands; generated gesture sequences exercise timer invariants.
- Keep Windows minimize handling owned by Settings, preserve file-dialog errors, and evaluate setup
  directory defaults only when needed.
- Run locked Rust, Python, shell, and documentation checks through mise. Verify native Rust on
  Windows and macOS, and build packages with owned-window rendering checks on all three platforms.
- Consolidate living documentation around ownership, current behavior, and relevant checks. Remove
  duplicate tray notification handling and update the dependency-patch instructions.

Windows x64 and Apple silicon macOS zip packages, plus Linux x86_64 AppImage and bundled tar
packages, are attached with SHA-256 checksums. Existing settings, engines, models, dictation
gestures, recognition policy, and the five-minute recording limit are preserved.

Linux remains experimental. Live microphone, keyboard-hook, clipboard/editor, and compositor
acceptance are separate from fake and owned-window checks. Forced native termination retains
synchronous cleanup; a stuck native driver can delay completion. Current contracts and measurement
limits are in [architecture](architecture.md) and [performance](performance.md).
