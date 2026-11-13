fn main() {
    println!("cargo::rustc-check-cfg=cfg(software)");
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let software = std::env::var_os("CARGO_FEATURE_SOFTWARE").is_some();
    // dav1d and libvpx back the platform codecs on Apple and Android, and build anywhere
    // with the `software` feature for their tests.
    if vendor == "apple" || os == "android" || software {
        println!("cargo::rustc-cfg=software");
    }
}
