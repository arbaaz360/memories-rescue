//! Print what `mr_composite::probe` reads from each file, as JSON.
//!
//!     cargo run -p mr-composite --example probe -- <file>...

fn main() {
    let mut failed = false;
    for arg in std::env::args_os().skip(1) {
        let path = std::path::PathBuf::from(&arg);
        match mr_composite::probe(&path) {
            Ok(info) => println!(
                "{}: {}",
                path.display(),
                serde_json::to_string_pretty(&info).expect("MediaInfo serialises")
            ),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
