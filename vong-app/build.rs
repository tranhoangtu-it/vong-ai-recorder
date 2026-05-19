//! Build script — compile Slint .slint files into Rust code +
//! embed the application icon into the Windows PE resource section.
//!
//! Slint compiles UI declarations ahead-of-time for performance.
//! On Windows, `winresource` puts `assets/icon.ico` (multi-size 16…256 px)
//! into the executable so File Explorer / taskbar / Alt-Tab shows the
//! branded violet "V" instead of the generic Rust binary glyph.

fn main() {
    slint_build::compile("ui/app-window.slint")
        .expect("Failed to compile Slint UI: app-window.slint");

    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "Vọng AI Recorder");
        res.set("ProductName", "Vọng AI Recorder");
        res.set("OriginalFilename", "vong.exe");
        res.set("LegalCopyright", "AGPL-3.0-only");
        if let Err(e) = res.compile() {
            // Non-fatal — keep building (icon will just be missing).
            eprintln!("warning: winresource icon embed failed: {e}");
        }
    }
}
