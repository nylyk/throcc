fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let resources = std::path::Path::new("../../packaging/windows");
        println!("cargo:rerun-if-changed=../../packaging/windows/throcc.rc");
        println!("cargo:rerun-if-changed=../../packaging/windows/throcc.ico");
        embed_resource::compile(
            resources.join("throcc.rc"),
            embed_resource::ParamsIncludeDirs([resources]),
        )
        .manifest_optional()
        .unwrap();
    }
}
