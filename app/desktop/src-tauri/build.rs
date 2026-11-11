fn main() {
    skills_list();
    // Only the app's own windows may call these commands (see
    // capabilities/); session windows load the local Columbus page and get
    // no access to the app at all, and neither does the companion page the
    // main window shows in a frame (a remote origin no capability names).
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "sessions",
            "open_session",
            "open_classic",
            "chat_open_map",
            "chat_settings",
            "chat_save_settings",
            "chat_set_key",
            "chat_clear_key",
            "chat_use_service",
            "chat_service_save",
            "chat_service_remove",
            "chat_models",
            "chat_other_apps",
            "chat_start",
            "chat_send",
            "chat_set_model",
            "projects_state",
            "project_save",
            "project_remove",
            "project_files",
            "project_files_open",
            "datagroups_state",
            "datagroup_save",
            "datagroup_remove",
            "registry_query",
            "share_export",
            "doc_preview",
            "note_confirm",
            "notes_check",
            "note_suggestions",
            "doc_import",
            "share_open",
            "share_import",
            "email_settings",
            "email_password_set",
            "share_my_key",
            "share_trust",
            "share_untrust",
            "share_settings",
            "share_team_folder",
            "share_team_list",
            "share_team_open",
            "org_tools_list",
            "project_dbs",
            "registry_update",
            "workflows_state",
            "workflow_install",
            "workflow_decline",
            "workflow_remove",
            "workflow_set_schedule",
            "workflow_run_now",
            "workflow_runs",
            "workflow_run_report",
            "chat_stop",
            "chat_answer",
            "chat_end",
            "chats_list",
            "chat_transcript",
            "chats_search",
            "chat_rename",
            "chat_delete",
            "open_settings",
            "settings_state",
            "settings_save_connection",
            "settings_delete_connection",
            "settings_set_switch",
            "settings_set_work_mode",
            "settings_pick",
            "settings_test_connection",
            "settings_draft_notes",
            "settings_tools",
            "settings_tool_install",
            "settings_tool_decline",
            "settings_tool_remove",
            "settings_tool_files",
            "toolboxes_state",
            "toolbox_install",
            "toolbox_add",
            "toolbox_remove",
            "toolbox_search",
            "toolbox_tool",
            "toolbox_pick",
            "toolbox_program_check",
            "settings_arcgis_signin",
            "map_layers_list",
            "map_layer_inspect",
            "map_layer_save",
            "map_layer_test",
            "map_layer_remove",
            "settings_environments",
            "settings_env_add",
            "settings_env_confirm",
            "settings_env_remove",
            "settings_env_approve",
            "settings_env_decline",
            "git_servers_state",
            "git_server_connect",
            "git_server_remove",
            "git_server_token_page",
            "git_server_pick_certificate",
            "sync_state",
            "sync_repos",
            "sync_branches",
            "sync_link",
            "sync_check",
            "sync_now",
            "sync_preview",
            "sync_preview_kept",
            "sync_resolve",
            "sync_send_anyway",
            "sync_history",
            "sync_disconnect",
            "sync_settings",
            "shell_info",
            "engine_check",
            "prefs_get",
            "prefs_set",
            "session_page",
            "session_files",
            "session_provenance",
            "sessions_live",
            "session_activity",
            "approvals_inbox",
            "approvals_open_browser",
            "audit_page",
            "audit_verify",
            "exports_list",
            "notes_list",
            "note_read",
            "note_mark_reviewed",
            "context_pack",
            "context_read",
            "project_databases",
            "skills_list",
        ]),
    ))
    .expect("tauri build");
}

/// The skills shipped in the repository (`skills/*/SKILL.md`), name and
/// description, for the Skills page when no installed copy is found.
fn skills_list() {
    use std::path::Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../skills");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let Ok(text) = std::fs::read_to_string(d.join("SKILL.md")) else {
                continue;
            };
            let mut name = None;
            let mut description = None;
            for l in text.lines().skip(1) {
                if l.trim() == "---" {
                    break;
                }
                if let Some(v) = l.strip_prefix("name:") {
                    name = Some(v.trim().to_string());
                }
                if let Some(v) = l.strip_prefix("description:") {
                    description = Some(v.trim().to_string());
                }
            }
            if let Some(name) = name {
                out.push(serde_json::json!({"name": name, "description": description}));
            }
        }
    }
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("skills.json");
    std::fs::write(dest, serde_json::to_string(&out).expect("json")).expect("write skills.json");
}
