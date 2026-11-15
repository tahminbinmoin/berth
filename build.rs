fn main() {
    println!("cargo:rerun-if-changed=assets/berth.rc");
    println!("cargo:rerun-if-changed=assets/berth.ico");
    println!("cargo:rerun-if-changed=assets/berth.exe.manifest");
    // Embeds the app icon, version info and the application manifest
    // (Common Controls v6, per-monitor DPI awareness, asInvoker).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        embed_resource::compile("assets/berth.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
