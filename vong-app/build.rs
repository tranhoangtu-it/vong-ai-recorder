//! Build script — compile Slint .slint files into Rust code.
//!
//! Slint compiles UI declarations ahead-of-time for performance.

fn main() {
    slint_build::compile("ui/app-window.slint")
        .expect("Failed to compile Slint UI: app-window.slint");
}
