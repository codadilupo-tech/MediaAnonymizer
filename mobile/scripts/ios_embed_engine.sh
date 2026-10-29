#!/bin/sh
# Xcode build phase of the Runner target ("Embed MediaAnonymizer engine").
#
# Builds crates/mobile for the platform being built and embeds it in the app
# as MediaAnonymizerCore.framework, which the Dart code opens with dlopen.
# ONNX Runtime is linked into it statically from Microsoft's official iOS
# xcframework, downloaded once into target/onnxruntime-ios/.
#
# Requirements: rustup/cargo in ~/.cargo/bin (or on PATH). Set
# ORT_IOS_XCFWK_PATH to use an onnxruntime.xcframework you already have.
set -eu

# Keep in sync with onnxRuntimeVersion in android/app/build.gradle.kts.
ORT_VERSION=1.30.0
FRAMEWORK=MediaAnonymizerCore
BUNDLE_ID=org.mediaanonymizer.core

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
min_ios=${IPHONEOS_DEPLOYMENT_TARGET:-16.0}

case "${PLATFORM_NAME:?run this from Xcode, or set PLATFORM_NAME, TARGET_BUILD_DIR and FRAMEWORKS_FOLDER_PATH}" in
    iphoneos) target=aarch64-apple-ios; platform=iPhoneOS ;;
    iphonesimulator)
        target=aarch64-apple-ios-sim; platform=iPhoneSimulator
        case " ${ARCHS:-arm64} " in
            *" arm64 "*) ;;
            *) echo "error: MediaAnonymizer runs on arm64 simulators only (Apple silicon Macs)" >&2; exit 1 ;;
        esac
        ;;
    *) echo "error: unsupported platform ${PLATFORM_NAME}" >&2; exit 1 ;;
esac

if [ -z "${ORT_IOS_XCFWK_PATH:-}" ]; then
    ort_dir="$repo/target/onnxruntime-ios/$ORT_VERSION"
    if [ -z "$(find "$ort_dir" -maxdepth 2 -name onnxruntime.xcframework -type d 2>/dev/null)" ]; then
        echo "Downloading ONNX Runtime $ORT_VERSION for iOS"
        mkdir -p "$ort_dir"
        curl --fail --silent --show-error --location --retry 3 -o "$ort_dir/pod.zip" \
            "https://download.onnxruntime.ai/pod-archive-onnxruntime-c-$ORT_VERSION.zip"
        unzip -q -o "$ort_dir/pod.zip" -d "$ort_dir"
        rm -f "$ort_dir/pod.zip"
    fi
    ORT_IOS_XCFWK_PATH=$(find "$ort_dir" -maxdepth 2 -name onnxruntime.xcframework -type d | head -n 1)
fi

# Xcode's build environment (SDKROOT, *_DEPLOYMENT_TARGET for other platforms,
# compiler flags...) breaks the host-side build scripts cargo compiles, so
# cargo runs with a clean environment.
path="${CARGO_HOME:-$HOME/.cargo}/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
run_clean() {
    env -i HOME="$HOME" USER="${USER:-}" TMPDIR="${TMPDIR:-/tmp}" PATH="$path" \
        ${CARGO_HOME:+CARGO_HOME="$CARGO_HOME"} ${RUSTUP_HOME:+RUSTUP_HOME="$RUSTUP_HOME"} \
        ${DEVELOPER_DIR:+DEVELOPER_DIR="$DEVELOPER_DIR"} \
        ORT_IOS_XCFWK_PATH="$ORT_IOS_XCFWK_PATH" IPHONEOS_DEPLOYMENT_TARGET="$min_ios" \
        "$@"
}

if command -v rustup >/dev/null 2>&1 || [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/rustup" ]; then
    run_clean rustup target add "$target" >/dev/null
fi
# Always optimised: the pixel kernels are far too slow in debug builds.
# ONNX Runtime's static framework also needs UIKit and Network, which ort-sys
# does not link.
run_clean cargo rustc --manifest-path "$repo/Cargo.toml" --release --locked \
    -p mediaanon-mobile --lib --crate-type cdylib --no-default-features --target "$target" \
    -- -C "link-arg=-Wl,-install_name,@rpath/$FRAMEWORK.framework/$FRAMEWORK" \
    -C link-arg=-framework -C link-arg=UIKit -C link-arg=-framework -C link-arg=Network

dest="${TARGET_BUILD_DIR:?}/${FRAMEWORKS_FOLDER_PATH:?}/$FRAMEWORK.framework"
rm -rf "$dest"
mkdir -p "$dest"
cp "$repo/target/$target/release/libmediaanon_mobile.dylib" "$dest/$FRAMEWORK"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n 1)
cat > "$dest/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>$FRAMEWORK</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$FRAMEWORK</string>
	<key>CFBundlePackageType</key>
	<string>FMWK</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>$platform</string>
	</array>
	<key>MinimumOSVersion</key>
	<string>$min_ios</string>
</dict>
</plist>
PLIST

if [ -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ] && [ "${CODE_SIGNING_ALLOWED:-YES}" != "NO" ]; then
    codesign --force --sign "$EXPANDED_CODE_SIGN_IDENTITY" ${OTHER_CODE_SIGN_FLAGS:-} -- "$dest"
fi
echo "Embedded $FRAMEWORK ($target) in $dest"
