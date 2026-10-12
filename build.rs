fn main() {
    assert!(
        std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos",
        "Luminator currently requires macOS"
    );
    cc::Build::new()
        .include("vendor/oil-find")
        .file("native/macos.c")
        .flag("-mmacosx-version-min=14.0")
        .compile("luminator_native");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=CoreServices");
    println!("cargo:rerun-if-changed=vendor/oil-find");
    println!("cargo:rerun-if-changed=native/macos.c");
}
