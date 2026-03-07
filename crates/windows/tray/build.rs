fn main() {
    // Embed Windows VERSIONINFO so the notification area and taskbar settings
    // show "FindAnything" instead of "find-tray.exe".
    //
    // CARGO_CFG_TARGET_OS is the compilation target ("windows"), not the host
    // OS — this is intentional: build.rs always runs on the host (Linux in the
    // cross-compilation flow) but we only want to embed resources for Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set("FileDescription", "FindAnything");
        res.set("ProductName", "Find Anything");
        res.set("CompanyName", "Jamie Treworgy");
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=winres failed to embed version info: {e}");
        }
    }
}
