use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(screen_capture_kit)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let abi = std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default();
    // The iOS simulator's SDK has no ScreenCaptureKit.
    if os == "macos" || (os == "ios" && abi != "sim") {
        println!("cargo::rustc-cfg=screen_capture_kit");
    }
    if os != "macos" {
        return;
    }
    // The ScreenCaptureKit bridge only searches Xcode's toolchain for the Swift
    // compatibility libraries, which a Command Line Tools install keeps elsewhere.
    let Ok(output) = Command::new("xcrun").args(["--find", "swift"]).output() else {
        return;
    };
    let swift = String::from_utf8_lossy(&output.stdout);
    let Some(toolchain) = Path::new(swift.trim()).parent().and_then(Path::parent) else {
        return;
    };
    let libraries = toolchain.join("lib/swift/macosx");
    if libraries.is_dir() {
        println!("cargo:rustc-link-search=native={}", libraries.display());
    }
}
