# Mobile app

`mobile/` is a [Flutter](https://flutter.dev) app for **Android** and **iOS**
built on the same engine as the CLI and the desktop app (`mediaanon-core`).
Photos are processed on the device: nothing is uploaded and there is no
telemetry.

## Features

- **Choose photos** from the library or **take a photo** with the camera, then
  **Anonymize** the whole list in one go. Each row shows the outcome (faces
  redacted, metadata blocks removed) or the error; failed photos can be retried.
- **Settings** (saved between runs), with the same meaning as on the desktop:
  - **Metadata**: *Keep*, *Remove sensitive* (GPS & personal data) or *Remove all*
    (see [PRIVACY.md](PRIVACY.md)). Removal never re-encodes the photo.
  - **Faces**: mode (*Eyes only* / *Full face*), style (*Black band* / *Blur* /
    *Pixelate*), shape, strength, padding and detection threshold.
  - **Timestamp**: burn the capture date into a corner, with a custom format and size.
  - **Save results to Photos**: copy every result to the photo library
    automatically (on by default). Results can also be shared from the app.
- **Photo details**: tap a photo to compare the *Original*, a live redacted
  *Preview* (updated when you change a setting) and the *Result*; see the
  metadata found in the original (sensitive entries flagged) and the report
  (faces, removed blocks, sizes).

JPEG faces are redacted in the DCT domain like everywhere else: the blocks that
do not touch a face are bit-identical to the original.

### Limitations

- **Photos only.** The video pipeline drives `ffmpeg`/`ffprobe` processes,
  which mobile platforms cannot spawn.
- **HEIC/HEIF/AVIF are not supported** by the engine. On iOS the photo picker
  converts HEIC photos to JPEG (keeping their metadata); on Android, photos
  shot in HEIC are rejected with an explanation.
- On Android, the system photo picker may remove GPS coordinates from the
  copies it hands to apps, so the metadata list can show less than the
  original file contains. The result is unaffected.
- Face detection runs on the CPU; GPU execution providers are desktop-only.

## Architecture

```text
mobile/lib (Dart UI)  ──dart:ffi──▶  crates/mobile (C ABI, JSON in/out)  ──▶  mediaanon-core
                                     libmediaanon_mobile.so (Android)
                                     MediaAnonymizerCore.framework (iOS)
```

- `crates/mobile` exposes a handful of C functions (`include/mediaanon.h`):
  `mediaanon_inspect`, `mediaanon_process`, `mediaanon_preview`, cancel
  handles and string helpers. Options are the JSON form of
  `mediaanon_core::Options`; results are `{"ok": ...}` or
  `{"error": {"kind", "message"}}`. Panics are caught. The face detector is
  loaded once and reused while the options stay the same.
- The Dart side (`lib/src/engine/`) calls these functions on background
  isolates, so the UI never blocks.
- **ONNX Runtime** (face detection):
  - Android: the `com.microsoft.onnxruntime:onnxruntime-android` AAR packages
    `libonnxruntime.so`; the Rust library is built with `ort-dynamic` and loads it
    at startup.
  - iOS: Microsoft's static `onnxruntime.xcframework` is linked into the
    framework (`ORT_IOS_XCFWK_PATH`).

  Both use the same version (`onnxRuntimeVersion` in
  `android/app/build.gradle.kts`, `ORT_VERSION` in `scripts/ios_embed_engine.sh`).

## Building

Requirements: [Flutter](https://docs.flutter.dev/get-started/install) (3.47 or
newer) and Rust via [rustup](https://rustup.rs) (1.92 or newer).

### Android

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk
cd mobile
flutter build apk --release      # or: flutter run
```

You also need the Android SDK and the NDK version Flutter asks for (Android
Studio → SDK Manager → *NDK (Side by side)*). Gradle runs `cargo ndk` before
every build (task `buildRust`), always in release mode, and packages the
libraries for `arm64-v8a`, `armeabi-v7a` and `x86_64`. Pass
`-Pmediaanon.skipRust=true` to package libraries you placed in
`build/app/rustJniLibs/<abi>/` yourself.

Release builds are signed with the debug key unless `MEDIAANON_KEYSTORE`,
`MEDIAANON_KEYSTORE_PASSWORD`, `MEDIAANON_KEY_ALIAS` and
`MEDIAANON_KEY_PASSWORD` point at your keystore (see
[RELEASING.md](RELEASING.md#mobile-apps)).

### iOS

On a Mac with Xcode:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cd mobile
flutter run                      # simulator or device
flutter build ipa                # App Store / TestFlight (needs a signing team)
```

The Runner target has an **Embed MediaAnonymizer engine** build phase
(`scripts/ios_embed_engine.sh`): it downloads ONNX Runtime for iOS once into
`target/onnxruntime-ios/`, builds `crates/mobile` for the device or the
simulator, and embeds and signs `MediaAnonymizerCore.framework`. Set
`ORT_IOS_XCFWK_PATH` to use an `onnxruntime.xcframework` you already have.
Simulators are supported on Apple silicon Macs. Set your team in Xcode
(*Runner → Signing & Capabilities*) to run on a device. The app requires iOS 16.

## Tests

```sh
cargo test -p mediaanon-mobile               # the C ABI
cargo build -p mediaanon-mobile              # host build for the Dart tests
cd mobile
MEDIAANON_MOBILE_LIB=../target/debug/libmediaanon_mobile.so flutter test
```

Without `MEDIAANON_MOBILE_LIB` (`.dylib` on macOS) the tests that call the
engine are skipped. The [Mobile workflow](../.github/workflows/mobile.yml)
runs them, then builds the Android APK and an unsigned iOS app; both are
uploaded as workflow artifacts. Releases attach them too (see
[RELEASING.md](RELEASING.md#mobile-apps)).

## Icons

`scripts/make_icons.py` redraws the desktop icon (`packaging/make_icons.py`)
as an opaque full-bleed square for iOS and as legacy and adaptive launcher
icons for Android.
