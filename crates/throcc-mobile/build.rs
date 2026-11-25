fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let abi = std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default();
    // The core reads ScreenCaptureKit's frame info on an iOS device, and iOS 26 has no
    // ScreenCaptureKit, so the library links it weakly, as the app does.
    if os == "ios" && abi != "sim" {
        println!("cargo::rustc-link-arg-cdylib=-Wl,-weak_framework,ScreenCaptureKit");
    }
}
