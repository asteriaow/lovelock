//! Embeds the application icon into the Windows executable so it shows in
//! Explorer, the taskbar and shortcuts. Other targets have nothing to embed.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // A missing resource compiler must not fail the build; the exe just keeps
    // the default icon, and the window icon still comes from the embedded PNG.
    if let Err(error) =
        embed_resource::compile("assets/app.rc", embed_resource::NONE).manifest_optional()
    {
        println!("cargo:warning=could not embed the exe icon: {error}");
    }
}
