fn main() {
    // Windows: with +crt-static (.cargo/config.toml, tools/package_checks.mjs remappedEnv) rustc
    // links the whole C runtime into the program. tauri-build's own way (staticVCRuntime: the VC
    // runtime static, the UCRT from the system's api-ms-win-crt-*.dll) would take the UCRT out of it
    // again, so it is off then; without +crt-static it stays as Tauri has it.
    let crt_static = std::env::var("CARGO_CFG_TARGET_FEATURE")
        .is_ok_and(|features| features.split(',').any(|f| f == "crt-static"));
    let mut attributes = tauri_build::Attributes::new();
    if crt_static {
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new().static_vc_runtime(false));
    }
    if let Err(error) = tauri_build::try_build(attributes) {
        println!("{error:#}");
        std::process::exit(1);
    }
}
