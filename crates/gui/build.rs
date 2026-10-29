//! Embeds the icon and version information into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/icon.ico")
            .set("ProductName", "MediaAnonymizer")
            .set("FileDescription", "MediaAnonymizer - remove metadata and redact faces")
            .set("LegalCopyright", "MediaAnonymizer contributors, MIT OR Apache-2.0");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cannot embed the Windows icon: {e}");
        }
    }
}
