fn main() {
    // On macOS, embed Info.plist into __TEXT,__info_plist so TCC (Local Network
    // privacy etc.) treats the bare binary as having its own bundle metadata.
    // When run from inside Apytti.app/Contents/MacOS/, the bundle's Info.plist
    // takes precedence; this fallback covers /usr/local/bin/apytti symlink
    // invocations and dev-mode `cargo run`. See Apple TN3179.
    //
    // Gate on CARGO_CFG_TARGET_OS (the cargo target, not the build host) so
    // cross-builds for linux/windows from macOS don't emit Mach-O linker args.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        use std::path::PathBuf;
        let version = env!("CARGO_PKG_VERSION");
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>net.calii.apytti.app</string>
<key>CFBundleName</key><string>apytti</string>
<key>CFBundleExecutable</key><string>apytti</string>
<key>CFBundleVersion</key><string>{v}</string>
<key>CFBundleShortVersionString</key><string>{v}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSUIElement</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
<key>NSAppTransportSecurity</key><dict><key>NSAllowsArbitraryLoads</key><true/></dict>
<key>NSLocalNetworkUsageDescription</key><string>Apytti reaches AI backends (Ollama, MCP servers) on your local network.</string>
<key>NSBonjourServices</key><array><string>_http._tcp</string><string>_https._tcp</string></array>
</dict></plist>"#,
            v = version
        );
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("Info.plist");
        std::fs::write(&out, plist).unwrap();
        println!("cargo:rustc-link-arg=-sectcreate");
        println!("cargo:rustc-link-arg=__TEXT");
        println!("cargo:rustc-link-arg=__info_plist");
        println!("cargo:rustc-link-arg={}", out.display());
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=Cargo.toml");
    }
}
