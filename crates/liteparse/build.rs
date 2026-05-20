use std::env;

fn main() {
    // DEP_PDFIUM_LIB_PATH is exported by pdfium-sys's build.rs via `cargo:lib_path=...`
    // Available because liteparse directly depends on pdfium-sys (links = "pdfium").
    if let Ok(lib_path) = env::var("DEP_PDFIUM_LIB_PATH") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_path}");
    }
}
