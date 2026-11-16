// Embeds the application icon and version information into the Windows executable.
fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../../packaging/windows/ncatimage.ico");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../packaging/windows/ncatimage.ico")
            .set("ProductName", "nCatImage")
            .set("FileDescription", "nCatImage")
            .set("OriginalFilename", "nCatImage.exe")
            .set("InternalName", "nCatImage");
        resource
            .compile()
            .expect("embed Windows resources (requires the Windows SDK's rc.exe)");
    }
}
