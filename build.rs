//! Compiles the Windows resources into the executable: the icon Explorer
//! shows on the file itself, and the details that appear under
//! right-click → Properties and in Task Manager.
//!
//! Without this the binary is anonymous — a generic icon and a blank Details
//! tab — even though the app sets its own window icon at runtime.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Vid Sizer");
        res.set("FileDescription", "Vid Sizer — resize, convert, crop and edit video");
        res.set("CompanyName", "Dan Koscielak");
        res.set("LegalCopyright", "© 2026 Dan Koscielak");
        res.set("OriginalFilename", "Vid Sizer.exe");
        res.set("InternalName", "vid-sizer");

        // A failed resource compile should not stop the build; the app still
        // runs, it just looks anonymous in Explorer.
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
