fn main() {
    // Lets the sidecar test find `binaries/<name>-<triple>` without guessing the triple.
    println!(
        "cargo:rustc-env=TARGET_TRIPLE={}",
        std::env::var("TARGET").expect("cargo sets TARGET")
    );
    tauri_build::build()
}
