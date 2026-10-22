//! Windows: the program's icon and version in Explorer and the taskbar (the C++'s resources/janus.rc.in).

fn main() {
    println!("cargo:rerun-if-changed=../../resources/janus.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    #[cfg(windows)]
    {
        let mut r = winresource::WindowsResource::new();
        r.set_icon("../../resources/janus.ico")
            .set("FileDescription", "Janus - raster time series analysis")
            .set("ProductName", "Janus")
            .set("InternalName", "janus");
        if let Err(e) = r.compile() {
            println!("cargo:warning=the program's icon and version were not embedded: {e}");
        }
    }
}
