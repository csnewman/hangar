fn main() {
    // The libraries bundled beside hangar-krun -- virglrenderer, and on the
    // Mac the Vulkan loader -- are found there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    } else {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
