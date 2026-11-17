fn main() {
    println!("cargo::rustc-check-cfg=cfg(software)");
    println!("cargo::rustc-check-cfg=cfg(ffmpeg)");
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let software = std::env::var_os("CARGO_FEATURE_SOFTWARE").is_some();
    // dav1d and libvpx back the platform codecs on Apple and Android, and build anywhere
    // with the `software` feature for their tests.
    let own_codecs = vendor == "apple" || os == "android";
    if own_codecs || software {
        println!("cargo::rustc-cfg=software");
    }
    // Apple and Android use their own codecs whether or not the feature is on.
    if !own_codecs && std::env::var_os("CARGO_FEATURE_FFMPEG").is_some() {
        println!("cargo::rustc-cfg=ffmpeg");
    }
}
