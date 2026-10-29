# Contributing to MediaAnonymizer

Thanks for helping! This project handles people's private data, so
correctness and conservative behaviour matter more than features.

## Getting started

```sh
git clone <repository-url> MediaAnonymizer
cd MediaAnonymizer
cargo test                      # core + CLI (video tests need ffmpeg/ffprobe on PATH)
cargo run -p mediaanon-cli -- process --help
cargo run -p mediaanon-gui
```

Requirements: Rust 1.92+ (1.95+ for the GUI), a C compiler (bundled libjpeg), FFmpeg 5.1+ for
video tests. The first build downloads ONNX Runtime; offline or custom setups
can use `--no-default-features --features ort-dynamic` with `ORT_DYLIB_PATH`
(see [docs/HARDWARE_ACCELERATION.md](docs/HARDWARE_ACCELERATION.md)).

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before larger changes.

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

* CI runs formatting, clippy and the full test suite on Linux (x86_64 and
  arm64), macOS (arm64) and Windows for every push and pull request. Put
  platform-specific checks in `crates/core/tests/platform.rs` behind
  `#[cfg(...)]`. Set `MEDIAANON_REQUIRE_FFMPEG=1` locally to make missing
  FFmpeg fail the video tests instead of skipping them.
* Add tests for new behaviour. Metadata rules need a unit test with a
  hand-built sample (see `metadata/exif.rs` and `metadata/jpeg.rs`), pipeline
  changes an integration test in `crates/core/tests`.
* Keep [docs/PRIVACY.md](docs/PRIVACY.md) in sync with any change to what is
  removed or kept, and update [CHANGELOG.md](CHANGELOG.md).
* Do not commit personal photos or videos. Test fixtures must be public domain
  or generated (document their origin in `crates/core/tests/fixtures/README.md`).
* Prefer removing data over keeping it when a format is ambiguous, and never
  silently fall back from an explicitly requested accelerator.

## Code style

* `rustfmt.toml` defines formatting (120 columns).
* Public items have doc comments; comments explain *why*, not *what*.
* No `unwrap()` on data coming from files or external processes.
* `unsafe` is confined to `photo/jpeg_dct.rs` (libjpeg FFI); keep it there and
  document invariants.

## Commit messages

Use the imperative mood (`Add WebP XMP filtering`), reference issues
(`Fixes #12`), and keep unrelated changes in separate commits.

## License

By contributing you agree that your contributions are dual-licensed under the
MIT and Apache-2.0 licenses, as the rest of the project.
