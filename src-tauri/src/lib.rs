//! Tauri shell for Memories Rescue. M0 ships a placeholder window only; the
//! processing core lives in `crates/mr-core` and is wired in from M1 onward.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
