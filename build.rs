//! Embed the Tern icon and version info into the Windows executable.
//! Best effort: if the resource compiler is missing the build still succeeds.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/tern.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/tern.ico");
    res.set("ProductName", "Tern");
    res.set("FileDescription", "Tern");
    res.set("CompanyName", "Tern");
    if let Err(e) = res.compile() {
        println!(
            "cargo:warning=Tern: could not embed the Windows icon ({e}); the app still works."
        );
    }
}
