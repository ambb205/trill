//! Gives the Windows executable its icon.

fn main() {
    const ICON: &str = "../../assets/trill.ico";
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(ICON);
    // The icon needs the Windows SDK's resource compiler. Without it the
    // program still builds and runs; it just has the default icon in
    // Explorer (the window sets its own icon at run time either way).
    if let Err(error) = resource.compile() {
        println!("cargo:warning=the executable has no icon: {error}");
    }
}
