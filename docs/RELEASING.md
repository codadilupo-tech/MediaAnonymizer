# Releasing

Releases are built by GitHub Actions ([`.github/workflows/release.yml`](../.github/workflows/release.yml)).
Pushing a version tag is all it takes: the workflow builds every platform,
creates a **draft** release, attaches the files and then publishes it.

The draft-first order is required by *immutable releases* (repository
*Settings → General → Releases*): once a release is published, no file can be
added to it. **Do not publish the release yourself before the workflow has
run**: it would have no assets and could not get any (the workflow stops with
an error right away in that case).

## Steps

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`), update
   `CHANGELOG.md`, commit and push to `main`. The workflow warns if the tag
   does not match the crate version.
2. Start the release, either way:
   * push a tag: `git tag v0.2.0 && git push origin v0.2.0`;
   * or **Actions → Release → Run workflow**, choose `main` and enter
     `v0.2.0`; the tag is created on that branch when the release is
     published.

   Release notes are this version's `## [<version>]` section of
   `CHANGELOG.md` (not GitHub's generated notes, which list the pull request
   authors).
3. Wait for the *Release* workflow (about 20–30 minutes). The release is then
   published with these files:

| File | Contents |
|---|---|
| `MediaAnonymizer-<v>-macos-arm64.dmg` | `MediaAnonymizer.app` (Apple silicon, macOS 13.4+), drag to Applications |
| `mediaanon-<v>-macos-arm64.tar.gz` | command-line tool |
| `MediaAnonymizer-<v>-windows-x64-setup.exe` | installer (per-user, no admin needed; optional desktop icon and PATH entry) |
| `MediaAnonymizer-<v>-windows-x64.zip` | portable GUI + CLI |
| `MediaAnonymizer-<v>-linux-x86_64.tar.gz`, `…-linux-arm64.tar.gz` | GUI + CLI |
| `MediaAnonymizer-<v>-android.apk` | mobile app for Android 7+ (arm64, armv7, x86_64), sideloadable |
| `MediaAnonymizer-<v>-ios-unsigned.ipa` | mobile app for iOS 16+, **unsigned** (re-sign it to install, see below) |
| `SHA256SUMS.txt` | checksums of all of the above |

Every push or pull request that changes `packaging/` or the workflow runs a
*dry run*: all installers are built and attached to the workflow run as
artifacts (nothing is published), so packaging changes can be checked
before a release.

If the workflow fails, fix it and run it again for the same tag (**Actions →
Release → Run workflow**): until the publish step succeeds the release is
still a draft, which can be refilled. A published release cannot be rebuilt;
release a new version (e.g. `v0.2.1`) instead.

There is no Intel-Mac build: `ort` ships no prebuilt ONNX Runtime for
`x86_64-apple-darwin`. Intel Mac users can build from source with
`--no-default-features --features ort-dynamic` and an ONNX Runtime from
`pip install onnxruntime` (see docs/HARDWARE_ACCELERATION.md).

Builds include the GPU inference provider of each platform: Core ML on macOS
(selectable, never automatic), DirectML on Windows (used by `auto`, with a
CPU fallback when no DirectX 12 GPU is usable). FFmpeg is **not**
bundled; users install it separately for video processing (photos work
without it).

## Anonymity

Releases must not identify their maintainer. The workflow creates and
publishes the release as `github-actions`, takes the notes from
`CHANGELOG.md`, and before publishing scans every file (inside archives, the
APK and the IPA too, and the `.app` before it goes into the DMG) for the
repository owner's name and for the strings in the optional
`RELEASE_FORBIDDEN_STRINGS` secret, one per line: add your real name, e-mail
addresses and other handles there. A match stops the release; the log names
the file, never the string. Locally:

```sh
MEDIAANON_FORBIDDEN=$'name\nemail@example.com' python3 scripts/check_release_anonymity.py <files or dirs>
```

What the workflow cannot hide:

* the repository URL and owner, and the account shown on each Actions run
  (who pushed or started it): publish from an account or organization that
  is not linked to you;
* git history: commit authors and e-mails, and the tagger of an annotated
  tag. Start releases with **Run workflow** (the tag is created by
  `github-actions`) rather than pushing a tag, and do not prepare the draft
  release yourself (its author would be you);
* signing identities: a *Developer ID* certificate and notarization are tied
  to the Apple developer account's legal name, and an Android keystore holds
  the name given when it was created. The scan stops a release signed with
  them if that name is in `RELEASE_FORBIDDEN_STRINGS`; leave those secrets
  unset, or use an organization's certificate and a keystore with a neutral
  name.

## macOS signing and notarization

Without configuration the app is *ad-hoc signed*. macOS then blocks the first
launch of a downloaded copy, often reporting that the app "is damaged". Since
macOS 15 right-click → **Open** no longer overrides this; users allow it once
with *System Settings → Privacy & Security → Open Anyway*, or from a terminal:

```sh
xattr -dr com.apple.quarantine /Applications/MediaAnonymizer.app
```

To ship a signed and notarized app that opens without warnings, add these
repository secrets (*Settings → Secrets and variables → Actions*). They require
an Apple Developer Program membership:

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE` | `base64 -i DeveloperID.p12` of a *Developer ID Application* certificate exported with its private key |
| `MACOS_CERTIFICATE_PASSWORD` | password of that `.p12` |
| `APPLE_ID` | Apple ID email of the developer account |
| `APPLE_TEAM_ID` | 10-character team ID |
| `APPLE_APP_PASSWORD` | an [app-specific password](https://support.apple.com/102654) for that Apple ID |

With the certificate the app is signed with the hardened runtime; with the
three `APPLE_*` secrets as well, the DMG is notarized and stapled.

## Mobile apps

The release builds the Flutter app in `mobile/` (see [MOBILE.md](MOBILE.md))
with the version from the tag (`--build-name`) and the workflow run number as
build number.

**Android**: without secrets the APK is signed with a throw-away debug key,
which is fine for sideloading but not for Google Play, and each release has a
different key (users must uninstall before upgrading). To sign with your own
key, add these repository secrets:

| Secret | Value |
|---|---|
| `ANDROID_KEYSTORE` | the keystore (`.jks`), base64-encoded: `base64 -i release.jks` |
| `ANDROID_KEYSTORE_PASSWORD` | keystore password |
| `ANDROID_KEY_ALIAS` | key alias |
| `ANDROID_KEY_PASSWORD` | key password |

Locally, the same values are read from the `MEDIAANON_KEYSTORE` (path),
`MEDIAANON_KEYSTORE_PASSWORD`, `MEDIAANON_KEY_ALIAS` and
`MEDIAANON_KEY_PASSWORD` environment variables.

**iOS**: the `.ipa` is unsigned, because installing on a device, TestFlight
and the App Store all need your team's certificates and provisioning
profiles. Re-sign it with your tooling, or build a signed archive on a Mac
with `flutter build ipa` after setting the team in Xcode.

## Windows signing

The installer and executables are not code-signed, so SmartScreen may show
"Windows protected your PC" on first run (**More info → Run anyway**). Signing
needs a code-signing certificate; it can be added to the workflow with
`signtool` or Azure Trusted Signing.

## Regenerating the icons

```sh
pip install pillow
python3 packaging/make_icons.py
```
