# Release notes

Speakeasy 0.3.3 removes standalone English “um” and “uh” locally before inserting dictation.

- **Remove um / uh** is enabled by default. Turn it off in Settings for literal transcription or
  non-English Parakeet dictation. Saving this preference keeps the model loaded.
- Both speech engines share a linear cleanup pass with at most one output allocation. Cleanup
  handles case and pause punctuation, preserves compounds and individually quoted tokens, and sends
  nothing for filler-only recognition.
- Whisper removes fillers only with its language set to English. Automatic and other language
  requests preserve literal words. Parakeet detects languages automatically; its cleanup switch
  expresses your English dictation preference.

Existing settings, engines, models, and dictation gestures are preserved. Recordings retain the
five-minute limit. Audio and recognition stay local, with no transcript history or uploads.

Windows x64 and Apple silicon macOS zip packages, plus experimental Linux x86_64 AppImage and
bundled tar packages, are attached with SHA-256 checksums. CI verifies all three platforms and runs
owned-window rendering checks. Live microphone, keyboard-hook, clipboard/editor, and compositor
acceptance remain separate; see [architecture](architecture.md) and [performance](performance.md).
