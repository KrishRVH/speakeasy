# Speakeasy GPUI media compatibility patch

This directory preserves the published [gpui_media 0.2.2 crate](https://crates.io/crates/gpui_media/0.2.2)
under Apache-2.0. Its archive SHA-256 is
`05cb8912ae17371725132d2b7eec6797a255accc95d58ee5c1134b529810f14b`.
`Cargo.toml.orig`, `build.rs`, source, and license preserve upstream provenance.
Registry cache markers are omitted.

The sole patch changes the macOS `core-foundation` requirement from `=0.10.0`
to `0.10.1`. GPUI's matching requirement is updated in `vendor/gpui/Cargo.toml`.
Without both changes Cargo cannot resolve the app's current stable
`core-foundation`: these are incompatible exact requirements in the same semver
series. No media implementation or generated bindings were changed.

The root Cargo override selects this copy outside the workspace. macOS CI builds
and tests the complete app, including GPUI and these media bindings. Linux can
cross-check the app's platform adapters, but cannot run the Xcode-dependent media
build. Remove this override and directory when a compatible GPUI/media release
accepts the current Core Foundation version and passes native app checks.
