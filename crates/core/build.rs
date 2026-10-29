//! Build script: macOS/iOS link fix for static ONNX Runtime builds.
//!
//! The static ONNX Runtime downloaded by `ort` (and the iOS xcframework used
//! by the mobile app) contains Objective-C code using
//! `@available(...)`, which compiles to calls to `___isPlatformVersionAtLeast`.
//! That function lives in clang's runtime library (`libclang_rt.osx.a`,
//! `libclang_rt.ios.a`, `libclang_rt.iossim.a`), which
//! clang links implicitly but rustc does not (it links with
//! `-nodefaultlibs`). Without it the final link fails with
//! "Undefined symbols: ___isPlatformVersionAtLeast". We locate the library
//! through the C compiler and link it explicitly. Elsewhere this is a no-op.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ABI");
    let simulator = std::env::var("CARGO_CFG_TARGET_ABI").as_deref() == Ok("sim");
    let runtime = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => "osx",
        Ok("ios") if simulator => "iossim",
        Ok("ios") => "ios",
        _ => return,
    };
    let lib = format!("libclang_rt.{runtime}.a");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let resource_dir = Command::new(&cc)
        .arg("--print-resource-dir")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    let Some(dir) = resource_dir.map(|d| d.join("lib").join("darwin")) else {
        println!(
            "cargo:warning=cannot query `{cc} --print-resource-dir`; linking may fail on ___isPlatformVersionAtLeast"
        );
        return;
    };
    if dir.join(&lib).is_file() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        // The iOS runtimes are multi-architecture archives, which rustc cannot
        // bundle into this rlib ("Unsupported archive identifier"): leave them
        // to the final link, whose linker picks the right slice.
        let kind = if runtime == "osx" { "static" } else { "static:-bundle" };
        println!("cargo:rustc-link-lib={kind}=clang_rt.{runtime}");
    } else {
        println!(
            "cargo:warning={} not found; install the Xcode Command Line Tools (`xcode-select --install`)",
            dir.join(&lib).display()
        );
    }
}
