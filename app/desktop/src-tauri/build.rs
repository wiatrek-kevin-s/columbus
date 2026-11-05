fn main() {
    // Only the app's own window may call these two commands (see
    // capabilities/main.json); session windows load the local Columbus page
    // and get no access to the app at all.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["sessions", "open_session"])),
    )
    .expect("tauri build");
}
