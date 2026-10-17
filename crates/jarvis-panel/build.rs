fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let manifest = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets its package directory"),
        );
        let plist = manifest.join("Info.plist");
        // Embed the explicit microphone purpose string in the native executable.
        // This is metadata only; loading the application never opens the mic.
        println!(
            "cargo:rustc-link-arg-bin=jarvis-panel=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
