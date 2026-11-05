fn main() {
    // Only the app's own windows may call these commands (see
    // capabilities/); session windows load the local Columbus page and get
    // no access to the app at all.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "sessions",
            "open_session",
            "open_chat",
            "chat_open_map",
            "chat_settings",
            "chat_save_settings",
            "chat_set_key",
            "chat_clear_key",
            "chat_start",
            "chat_send",
            "chat_stop",
            "chat_answer",
            "chat_end",
            "open_settings",
            "settings_state",
            "settings_save_connection",
            "settings_delete_connection",
            "settings_set_switch",
            "settings_pick",
            "settings_test_connection",
        ]),
    ))
    .expect("tauri build");
}
