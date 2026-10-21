//! Embeds the PDFflow icon and version information in the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "PDFflow")
        .set("FileDescription", "PDFflow PDF editor")
        .set("LegalCopyright", "MIT OR Apache-2.0");
    // Cross-compiling from macOS or Linux uses the MinGW resource compiler.
    if !cfg!(windows) {
        res.set_windres_path("x86_64-w64-mingw32-windres").set_ar_path("x86_64-w64-mingw32-ar");
    }
    if let Err(e) = res.compile() {
        println!("cargo:warning=icon not embedded: {e}");
    }
}
