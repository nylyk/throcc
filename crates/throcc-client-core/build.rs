fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(screen_capture_kit)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let abi = std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default();
    // The iOS simulator's SDK has no ScreenCaptureKit.
    if os == "macos" || (os == "ios" && abi != "sim") {
        println!("cargo::rustc-cfg=screen_capture_kit");
    }
}
