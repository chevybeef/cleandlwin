fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("app.manifest");
        println!("cargo:rustc-link-arg-bin=cleandlwin=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=cleandlwin=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
