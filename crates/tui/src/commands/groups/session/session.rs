//! Session commands: save, load, compact, export

use std::path::PathBuf;

use crate::session_manager::{
    create_saved_session_with_id_and_mode, create_saved_session_with_mode,
};
use crate::tui::app::{App, AppAction, ToolDetailRecord};
use crate::tui::session_picker::SessionPickerView;

use super::CommandResult;

/// Save session to file.
///
/// When an explicit path is given, the session is exported there
/// (user-visible explicit export).  Without a path, v0.8.44 saves
/// into the managed session directory (`~/.codewhale/sessions`
/// or legacy `~/.deepseek/sessions`) so repo-local `session_*.json`
/// artifacts are no longer created by default.
pub fn save(app: &mut App, path: Option<&str>) -> CommandResult {
    let explicit_save_path = path.map(PathBuf::from);

    let messages = app.api_messages.clone();
    let mut session = create_saved_session_with_mode(
        &messages,
        &app.model,
        &app.workspace,
        u64::from(app.session.total_tokens),
        app.system_prompt.as_ref(),
        Some(app.mode.label()),
    );
    session
        .metadata
        .set_model_provider_route(app.api_provider.as_str(), app.provider_id_for_persistence());
    app.sync_cost_to_metadata(&mut session.metadata);
    session.context_references = app.session_context_references.clone();
    session.artifacts = app.session_artifacts.clone();
    session.work_state = match app.work_state_snapshot() {
        Ok(state) => state,
        Err(err) => return CommandResult::error(format!("Failed to snapshot Work state: {err}")),
    };
    session.last_auto_route = app.auto_route_for_persistence();
    let save_path = explicit_save_path.unwrap_or_else(|| {
        let dir = crate::session_manager::default_sessions_dir()
            .unwrap_or_else(|_| app.workspace.clone());
        dir.join(format!("{}.json", session.metadata.id))
    });

    let sessions_dir = save_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| app.workspace.clone(), std::path::Path::to_path_buf);

    match std::fs::create_dir_all(&sessions_dir) {
        Ok(()) => {
            let json = match serde_json::to_string_pretty(&session) {
                Ok(j) => j,
                Err(e) => return CommandResult::error(format!("Failed to serialize session: {e}")),
            };
            match crate::utils::write_atomic(&save_path, json.as_bytes()) {
                Ok(()) => {
                    app.current_session_id = Some(session.metadata.id.clone());
                    app.current_session_metadata = Some(session.metadata.clone());
                    app.session_title = Some(session.metadata.title.clone());
                    if let Err(err) = app.publish_pending_work_state() {
                        return CommandResult::error(format!(
                            "Session saved, but Work views were not published: {err}"
                        ));
                    }
                    CommandResult::message(format!(
                        "Session saved to {} (ID: {})",
                        save_path.display(),
                        crate::session_manager::truncate_id(&session.metadata.id)
                    ))
                }
                Err(e) => CommandResult::error(format!("Failed to save session: {e}")),
            }
        }
        Err(e) => CommandResult::error(format!("Failed to create directory: {e}")),
    }
}

/// Fork the active conversation into a new saved sibling session and switch to it.
pub fn fork(app: &mut App) -> CommandResult {
    if app.session_transition_blocked() {
        return CommandResult::error(
            "Cannot fork a session while runtime work is active. Wait for the current turn, maintenance, and background tasks to finish, or cancel that specific work first.",
        );
    }
    if app.api_messages.is_empty() {
        return CommandResult::error("Nothing to fork. Send or load a message first.");
    }

    let manager = match crate::session_manager::SessionManager::default_location() {
        Ok(manager) => manager,
        Err(err) => {
            return CommandResult::error(format!("could not open sessions directory: {err}"));
        }
    };

    let parent_id = app
        .current_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut parent = create_saved_session_with_id_and_mode(
        parent_id,
        &app.api_messages,
        &app.model,
        &app.workspace,
        u64::from(app.session.total_tokens),
        app.system_prompt.as_ref(),
        Some(app.mode.label()),
    );
    parent
        .metadata
        .set_model_provider_route(app.api_provider.as_str(), app.provider_id_for_persistence());
    if let Some(cached) = app
        .current_session_metadata
        .as_ref()
        .filter(|metadata| metadata.id == parent.metadata.id)
    {
        parent.metadata.created_at = cached.created_at;
        parent.metadata.title.clone_from(&cached.title);
        parent
            .metadata
            .parent_session_id
            .clone_from(&cached.parent_session_id);
        parent.metadata.forked_from_message_count = cached.forked_from_message_count;
    }
    app.sync_cost_to_metadata(&mut parent.metadata);
    parent.context_references = app.session_context_references.clone();
    parent.artifacts = app.session_artifacts.clone();
    let work_state = match app.work_state_snapshot() {
        Ok(state) => state,
        Err(err) => return CommandResult::error(format!("Failed to snapshot Work state: {err}")),
    };
    parent.work_state = work_state.clone();
    parent.last_auto_route = app.auto_route_for_persistence();

    if let Err(err) = manager.save_session(&parent) {
        return CommandResult::error(format!("Failed to save parent session: {err}"));
    }

    let mut forked = create_saved_session_with_mode(
        &app.api_messages,
        &app.model,
        &app.workspace,
        u64::from(app.session.total_tokens),
        app.system_prompt.as_ref(),
        Some(app.mode.label()),
    );
    forked
        .metadata
        .set_model_provider_route(app.api_provider.as_str(), app.provider_id_for_persistence());
    forked.metadata.copy_cost_from(&parent.metadata);
    forked.metadata.mark_forked_from(&parent.metadata);
    forked.context_references = app.session_context_references.clone();
    forked.artifacts = match crate::artifacts::clone_artifact_records_for_session(
        &app.session_artifacts,
        &parent.metadata.id,
        &forked.metadata.id,
    ) {
        Ok(artifacts) => artifacts,
        Err(err) => {
            return CommandResult::error(format!(
                "Failed to retain exact evidence in forked session: {err}"
            ));
        }
    };
    forked.work_state = work_state;
    forked.last_auto_route = app.auto_route_for_persistence();

    if let Err(err) = manager.save_session(&forked) {
        return CommandResult::error(format!("Failed to save forked session: {err}"));
    }
    if let Err(err) = app.publish_pending_work_state() {
        return CommandResult::error(format!(
            "Sessions saved, but Work views were not published: {err}"
        ));
    }

    app.current_session_id = Some(forked.metadata.id.clone());
    app.current_session_metadata = Some(forked.metadata.clone());
    app.session_title = Some(forked.metadata.title.clone());
    app.session_artifacts = forked.artifacts.clone();
    rebind_forked_tool_details(
        app,
        &parent.metadata.id,
        &forked.metadata.id,
        &forked.artifacts,
    );
    let fork_id = forked.metadata.id.clone();
    let parent_label = crate::session_manager::truncate_id(&parent.metadata.id).to_string();
    let fork_label = crate::session_manager::truncate_id(&fork_id).to_string();

    CommandResult::with_message_and_action(
        format!("Forked session {parent_label} -> {fork_label}"),
        AppAction::SyncSession {
            session_id: Some(fork_id),
            messages: app.api_messages.clone(),
            system_prompt: app.system_prompt.clone(),
            model: app.model.clone(),
            workspace: app.workspace.clone(),
            mode: app.mode,
        },
    )
}

/// Point the live detail index at the fork's copied artifact namespace.
///
/// The transcript itself is intentionally retained in place during `/fork`,
/// so rebuilding details from serialized messages would be disruptive. Rebind
/// every known detail by its stable tool-call id instead. Any unmatched or
/// cross-session detail fails closed rather than retaining a hidden dependency
/// on the parent session.
fn rebind_forked_tool_details(
    app: &mut App,
    source_session_id: &str,
    target_session_id: &str,
    cloned_artifacts: &[crate::artifacts::ArtifactRecord],
) {
    fn rebind_one(
        detail: &mut ToolDetailRecord,
        source_session_id: &str,
        target_session_id: &str,
        cloned_artifacts: &[crate::artifacts::ArtifactRecord],
    ) {
        let Some(detail_artifact) = detail.artifact.as_mut() else {
            return;
        };
        let source_matches = detail_artifact.session_id.trim().is_empty()
            || detail_artifact.session_id == source_session_id;
        let cloned = source_matches
            .then(|| {
                cloned_artifacts
                    .iter()
                    .find(|artifact| artifact.tool_call_id == detail.tool_id)
            })
            .flatten();

        detail_artifact.session_id = target_session_id.to_string();
        detail_artifact.absolute_path = None;
        let Some(cloned) = cloned else {
            detail_artifact.relative_path = None;
            detail_artifact.available = false;
            return;
        };
        detail_artifact.relative_path = Some(cloned.storage_path.clone());
        detail_artifact.sha256 = crate::artifacts::adaptive_sha_from_artifact_id(&cloned.id)
            .or_else(|| detail_artifact.sha256.clone());
        detail_artifact.byte_size = cloned.byte_size;
        detail_artifact.available = crate::artifacts::open_session_artifact_for_read(
            target_session_id,
            &cloned.storage_path,
        )
        .is_ok();
    }

    for details in app.tool_details_by_cell.values_mut() {
        for detail in details {
            rebind_one(
                detail,
                source_session_id,
                target_session_id,
                cloned_artifacts,
            );
        }
    }
    for detail in app.active_tool_details.values_mut() {
        rebind_one(
            detail,
            source_session_id,
            target_session_id,
            cloned_artifacts,
        );
    }
}

/// Start a fresh saved session from the current TUI state.
pub fn new_session(app: &mut App, arg: Option<&str>) -> CommandResult {
    let force = match arg.map(str::trim).filter(|s| !s.is_empty()) {
        None => false,
        Some("--force" | "force") => true,
        Some(other) => {
            return CommandResult::error(format!(
                "Usage: /new [--force]\n\nUnknown argument: {other}"
            ));
        }
    };

    if app.session_transition_blocked() {
        return CommandResult::error(
            "Cannot start a new session while runtime work is active. Wait for the current turn, maintenance, and background tasks to finish, or cancel that specific work. `/new --force` only discards draft or queued input.",
        );
    }

    if !force {
        let blockers = new_session_blockers(app);
        if !blockers.is_empty() {
            return CommandResult::error(format!(
                "Cannot start a new session while {}. Run `/new --force` to discard pending work and start a fresh session.",
                blockers.join(", ")
            ));
        }
    }

    let new_id = uuid::Uuid::new_v4().to_string();
    if !super::super::core::reset_conversation_state(app) {
        return CommandResult::error(
            "Could not start a new session because Work state is busy; retry in a moment.",
        );
    }
    app.clear_input();
    app.session_artifacts.clear();
    app.session_context_references.clear();
    app.tool_evidence.clear();
    app.current_session_id = Some(new_id.clone());
    app.current_session_metadata = None;
    app.session_title = Some("New Session".to_string());
    app.scroll_to_bottom();

    CommandResult::with_message_and_action(
        format!(
            "Started new session {} (New Session). Previous sessions remain available via /resume.",
            crate::session_manager::truncate_id(&new_id)
        ),
        AppAction::SyncSession {
            session_id: Some(new_id),
            messages: Vec::new(),
            system_prompt: None,
            model: app.model.clone(),
            workspace: app.workspace.clone(),
            mode: app.mode,
        },
    )
}

fn new_session_blockers(app: &App) -> Vec<&'static str> {
    let mut blockers = Vec::new();
    if !app.input.trim().is_empty() {
        blockers.push("the composer has unsent text");
    }
    if !app.queued_messages.is_empty() || app.queued_draft.is_some() {
        blockers.push("queued messages are pending");
    }
    blockers
}

/// Load session from file
pub fn load(app: &mut App, path: Option<&str>) -> CommandResult {
    if app.session_transition_blocked() {
        return CommandResult::error(
            "Cannot load a session while runtime work is active. Wait for the current turn, maintenance, and background tasks to finish, or cancel that specific work first.",
        );
    }
    let load_path = if let Some(p) = path {
        if p.contains('/') || p.contains('\\') {
            PathBuf::from(p)
        } else {
            app.workspace.join(p)
        }
    } else {
        return CommandResult::error("Usage: /load <path>");
    };

    let content = match std::fs::read_to_string(&load_path) {
        Ok(c) => c,
        Err(e) => {
            return CommandResult::error(format!("Failed to read session file: {e}"));
        }
    };

    let _session: crate::session_manager::SavedSession = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            return CommandResult::error(format!("Failed to parse session file: {e}"));
        }
    };

    // The command layer only validates the file shape. The event loop reloads
    // Config once and applies the session plus route atomically before it
    // rebuilds or syncs the engine.
    // Success is reported only after the event loop re-reads live Config and
    // atomically applies the session route. Emitting it here would leave a
    // false receipt in the current transcript if that final validation fails.
    CommandResult::action(crate::tui::app::AppAction::LoadSession(load_path))
}

/// Trigger context compaction
pub fn compact(_app: &mut App) -> CommandResult {
    // Trigger immediate compaction via engine
    CommandResult::with_message_and_action(
        "Context compaction triggered...".to_string(),
        AppAction::CompactContext,
    )
}

/// Trigger agent-driven context purging.
pub fn purge(_app: &mut App) -> CommandResult {
    CommandResult::with_message_and_action(
        "Agent context purge triggered...".to_string(),
        AppAction::PurgeContext,
    )
}

/// Open the session picker UI, or run a sub-action like
/// `prune <days>` for housekeeping (#406 phase-1.5).
pub fn sessions(app: &mut App, arg: Option<&str>) -> CommandResult {
    let trimmed = arg.unwrap_or("").trim();
    if trimmed.is_empty() {
        app.view_stack
            .push(SessionPickerView::new(&app.workspace, app.ui_locale));
        return CommandResult::ok();
    }

    let mut parts = trimmed.split_whitespace();
    let action = parts.next().unwrap_or("").to_ascii_lowercase();
    match action.as_str() {
        "prune" => prune(app, parts.next()),
        "show" | "list" | "picker" => {
            app.view_stack
                .push(SessionPickerView::new(&app.workspace, app.ui_locale));
            CommandResult::ok()
        }
        _ => CommandResult::error(format!(
            "unknown subcommand `{action}`. usage: /sessions [show|prune <days>]"
        )),
    }
}

/// Prune persisted sessions older than `<days>` from
/// `~/.deepseek/sessions/`. Wraps
/// [`crate::session_manager::SessionManager::prune_sessions_older_than`]
/// so users can run a safe cleanup without leaving the TUI. Skips
/// the checkpoint subdirectory (the helper guarantees that already).
fn prune(app: &mut App, days_arg: Option<&str>) -> CommandResult {
    let days_str = match days_arg {
        Some(s) => s,
        None => {
            return CommandResult::error(
                "usage: /sessions prune <days>   (e.g. `/sessions prune 30` to drop sessions older than 30 days)",
            );
        }
    };
    let days: u64 = match days_str.parse() {
        Ok(n) if n > 0 => n,
        _ => {
            return CommandResult::error(format!(
                "expected a positive integer number of days, got `{days_str}`"
            ));
        }
    };

    let manager = match crate::session_manager::SessionManager::default_location() {
        Ok(m) => m,
        Err(err) => {
            return CommandResult::error(format!("could not open sessions directory: {err}"));
        }
    };

    let max_age = std::time::Duration::from_secs(days.saturating_mul(24 * 60 * 60));
    // Never prune the active session, even if its timestamp is stale (a
    // just-resumed session isn't re-saved until its first post-resume write).
    let keep = app.current_session_id.as_deref();
    match manager.prune_sessions_older_than_keeping(max_age, keep) {
        Ok(0) => CommandResult::message(format!("no sessions older than {days}d to prune")),
        Ok(n) => CommandResult::message(format!(
            "pruned {n} session{} older than {days}d",
            if n == 1 { "" } else { "s" }
        )),
        Err(err) => CommandResult::error(format!("prune failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_support::EnvVarGuard;
    use crate::tui::app::{
        App, AppMode, ReasoningEffort, ToolDetailArtifact, TuiOptions, TurnCacheRecord,
    };
    use crate::tui::history::{GenericToolCell, HistoryCell, ToolCell, ToolStatus};
    use crate::tui::pager::PagerView;
    use std::time::Instant;
    use tempfile::TempDir;

    fn create_test_app_with_tmpdir(tmpdir: &TempDir) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: tmpdir.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn test_save_creates_file_and_sets_session_id() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let save_path = tmpdir.path().join("test_session.json");

        let result = save(&mut app, Some(save_path.to_str().unwrap()));
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(msg.contains("Session saved to"));
        assert!(msg.contains("ID:"));
        assert!(app.current_session_id.is_some());
        assert!(save_path.exists());
    }

    #[test]
    fn save_preserves_artifact_registry() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let save_path = tmpdir.path().join("artifact_session.json");
        app.session_artifacts
            .push(crate::artifacts::ArtifactRecord {
                id: "art_call_big".to_string(),
                kind: crate::artifacts::ArtifactKind::ToolOutput,
                session_id: "artifact-session".to_string(),
                tool_call_id: "call-big".to_string(),
                tool_name: "exec_shell".to_string(),
                success: Some(true),
                created_at: chrono::Utc::now(),
                byte_size: 512_000,
                preview: "cargo test output".to_string(),
                storage_path: tmpdir.path().join("call-big.txt"),
            });

        let result = save(&mut app, Some(save_path.to_str().unwrap()));

        assert!(!result.is_error);
        let saved: crate::session_manager::SavedSession =
            serde_json::from_str(&std::fs::read_to_string(save_path).unwrap()).unwrap();
        assert_eq!(saved.artifacts, app.session_artifacts);
    }

    #[test]
    fn save_preserves_latest_auto_route_receipt() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let save_path = tmpdir.path().join("auto_route_session.json");
        let receipt = crate::model_routing::AutoRouteReceipt {
            tier: crate::model_routing::AutoRouteTier::Fast,
            pair: crate::model_routing::AutoRoutePair {
                strong: crate::config::ZAI_GLM_5_2_MODEL.to_string(),
                fast: Some(crate::config::ZAI_GLM_5_TURBO_MODEL.to_string()),
            },
            scope: crate::model_routing::AutoRouteScope::ResolvedProvider,
            data_path: crate::model_routing::AutoRouteDataPath::LocalHeuristic,
            reason: crate::model_routing::AutoRouteReason::LocalHeuristic(
                crate::model_routing::AutoRouteHeuristicReason::ShortRequest,
            ),
        };
        app.set_model_selection("auto".to_string());
        app.last_effective_provider = Some(crate::config::ApiProvider::Zai);
        app.last_effective_provider_identity = Some("zai".to_string());
        app.last_effective_model = Some(crate::config::ZAI_GLM_5_TURBO_MODEL.to_string());
        app.last_auto_route_receipt = Some(receipt.clone());

        let result = save(&mut app, Some(save_path.to_str().unwrap()));

        assert!(!result.is_error);
        let saved: crate::session_manager::SavedSession =
            serde_json::from_str(&std::fs::read_to_string(save_path).unwrap()).unwrap();
        let route = saved.last_auto_route.expect("latest Auto route");
        assert_eq!(route.provider, crate::config::ApiProvider::Zai);
        assert_eq!(route.provider_identity, "zai");
        assert_eq!(route.model, crate::config::ZAI_GLM_5_TURBO_MODEL);
        assert_eq!(route.receipt, receipt);
    }

    #[test]
    fn fork_saves_parent_and_switches_to_child_session() {
        use std::io::Read as _;

        let _artifact_lock = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmpdir = TempDir::new().unwrap();
        let _lock = crate::test_support::lock_test_env();
        let home = tmpdir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let home_guard = EnvVarGuard::set("HOME", &home);
        let previous_home = home_guard.previous();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.set_provider_identity(crate::config::ApiProvider::Custom, "lm-studio");
        app.current_session_id = Some("parent-session".to_string());
        let mut cached_parent = create_saved_session_with_id_and_mode(
            "parent-session".to_string(),
            &[],
            &app.model,
            &app.workspace,
            0,
            None,
            Some(app.mode.label()),
        )
        .metadata;
        cached_parent.title = "Custom Parent".to_string();
        cached_parent.created_at = "2026-01-02T03:04:05Z"
            .parse()
            .expect("fixed parent timestamp");
        app.current_session_metadata = Some(cached_parent.clone());
        app.session_title = Some(cached_parent.title.clone());
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "try another path".to_string(),
                cache_control: None,
            }],
        });
        {
            let mut todos = app.todos.try_lock().expect("todos lock");
            todos.add(
                "preserve fork Work".to_string(),
                crate::tools::todo::TodoStatus::InProgress,
            );
        }
        {
            let mut plan = app.plan_state.try_lock().expect("plan lock");
            plan.update(crate::tools::plan::UpdatePlanArgs {
                objective: Some("Fork without Work drift".to_string()),
                ..crate::tools::plan::UpdatePlanArgs::default()
            });
        }
        app.cycle_effort();
        let expected_work = app
            .work_state_snapshot()
            .expect("Work snapshot")
            .expect("graph-backed Work state");
        assert!(
            expected_work.graph.is_some(),
            "fork fixture must use a graph"
        );
        let raw = "CW_FORK_RETAINED_EXACT_EVIDENCE\n";
        let sha = crate::hashing::sha256_hex(raw.as_bytes());
        let artifact_id = format!("art_output_{sha}_0123456789ab");
        let (parent_artifact_path, relative_path) =
            crate::artifacts::write_session_artifact("parent-session", &artifact_id, raw)
                .expect("write parent evidence");
        app.session_artifacts
            .push(crate::artifacts::ArtifactRecord {
                id: artifact_id,
                kind: crate::artifacts::ArtifactKind::ToolOutput,
                session_id: "parent-session".to_string(),
                tool_call_id: "call-fork-evidence".to_string(),
                tool_name: "run_tests".to_string(),
                success: Some(false),
                created_at: chrono::Utc::now(),
                byte_size: raw.len() as u64,
                preview: "failed output".to_string(),
                storage_path: relative_path,
            });
        let nested_raw = "CW_FORK_RETAINED_NESTED_EVIDENCE\n";
        let nested_sha = crate::hashing::sha256_hex(nested_raw.as_bytes());
        let nested_artifact_id = format!("art_output_{nested_sha}_fedcba987654");
        let (parent_nested_path, nested_relative_path) = crate::artifacts::write_session_artifact(
            "parent-session",
            &nested_artifact_id,
            nested_raw,
        )
        .expect("write nested parent evidence");
        app.session_artifacts
            .push(crate::artifacts::ArtifactRecord {
                id: nested_artifact_id,
                kind: crate::artifacts::ArtifactKind::ToolOutput,
                session_id: "parent-session".to_string(),
                tool_call_id: "call-fork-evidence.0".to_string(),
                tool_name: "read_file".to_string(),
                success: Some(true),
                created_at: chrono::Utc::now(),
                byte_size: nested_raw.len() as u64,
                preview: "nested output".to_string(),
                storage_path: nested_relative_path,
            });
        app.history = vec![HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "run_tests".to_string(),
            status: ToolStatus::Failed,
            input_summary: Some("cargo test".to_string()),
            output: Some("{\"status\":\"failed\"}".to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: Some("1 failure · output kept".to_string()),
            is_diff: false,
        }))];
        app.tool_details_by_cell.insert(
            0,
            vec![ToolDetailRecord {
                tool_id: "call-fork-evidence".to_string(),
                tool_name: "run_tests".to_string(),
                input: serde_json::json!({"command": "cargo test"}),
                output: Some("{\"status\":\"failed\"}".to_string()),
                artifact: Some(ToolDetailArtifact {
                    session_id: "parent-session".to_string(),
                    relative_path: Some(app.session_artifacts[0].storage_path.clone()),
                    absolute_path: Some(parent_artifact_path.clone()),
                    sha256: Some(sha.clone()),
                    byte_size: raw.len() as u64,
                    duration_ms: Some(17),
                    available: true,
                }),
            }],
        );
        app.resync_history_revisions();

        let result = fork(&mut app);

        assert!(!result.is_error, "{:?}", result.message);
        let new_id = app.current_session_id.clone().expect("fork session id");
        assert_ne!(new_id, "parent-session");
        assert!(result.message.as_deref().unwrap_or("").contains("Forked"));
        assert!(matches!(result.action, Some(AppAction::SyncSession { .. })));

        let manager = crate::session_manager::SessionManager::default_location().unwrap();
        let parent = manager
            .load_session("parent-session")
            .expect("parent saved");
        let child = manager.load_session(&new_id).expect("child saved");
        assert_eq!(parent.messages.len(), 1);
        assert_eq!(parent.metadata.model_provider, "custom");
        assert_eq!(
            parent.metadata.model_provider_id.as_deref(),
            Some("lm-studio")
        );
        assert_eq!(parent.metadata.title, cached_parent.title);
        assert_eq!(parent.metadata.created_at, cached_parent.created_at);
        assert_eq!(
            child.metadata.parent_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(child.metadata.forked_from_message_count, Some(1));
        assert_eq!(child.metadata.model_provider, "custom");
        assert_eq!(
            child.metadata.model_provider_id.as_deref(),
            Some("lm-studio")
        );
        assert_eq!(parent.work_state.as_ref(), Some(&expected_work));
        assert_eq!(child.work_state.as_ref(), Some(&expected_work));
        assert_eq!(child.artifacts.len(), 2);
        assert!(
            child
                .artifacts
                .iter()
                .all(|artifact| artifact.session_id == new_id)
        );
        assert_eq!(app.session_artifacts, child.artifacts);
        let rebound = app.tool_details_by_cell[&0]
            .first()
            .unwrap()
            .artifact
            .as_ref()
            .unwrap();
        assert_eq!(rebound.session_id, new_id);
        assert_eq!(
            rebound.relative_path.as_ref(),
            Some(&child.artifacts[0].storage_path)
        );
        assert!(rebound.absolute_path.is_none());
        let mut child_evidence = crate::artifacts::open_session_artifact_for_read(
            &new_id,
            &child.artifacts[0].storage_path,
        )
        .expect("forked evidence remains session-local");
        let mut copied = String::new();
        child_evidence.read_to_string(&mut copied).unwrap();
        assert_eq!(copied, raw);
        let nested_child = child
            .artifacts
            .iter()
            .find(|artifact| artifact.tool_call_id == "call-fork-evidence.0")
            .expect("nested child artifact");
        let mut nested_evidence =
            crate::artifacts::open_session_artifact_for_read(&new_id, &nested_child.storage_path)
                .expect("forked nested evidence remains session-local");
        let mut nested_copied = String::new();
        nested_evidence.read_to_string(&mut nested_copied).unwrap();
        assert_eq!(nested_copied, nested_raw);
        std::fs::remove_file(parent_artifact_path).expect("simulate parent evidence pruning");
        std::fs::remove_file(parent_nested_path).expect("simulate nested parent evidence pruning");
        assert!(crate::tui::ui::open_details_pager_for_cell(&mut app, 0));
        let mut view = app.view_stack.pop().expect("forked exact detail pager");
        let pager = view
            .as_any_mut()
            .downcast_mut::<PagerView>()
            .expect("pager");
        assert!(
            pager
                .body_text()
                .contains("CW_FORK_RETAINED_EXACT_EVIDENCE")
        );
        let cached_child = app
            .current_session_metadata
            .as_ref()
            .expect("child metadata cached");
        assert_eq!(cached_child.id, child.metadata.id);
        assert_eq!(cached_child.title, child.metadata.title);
        assert_eq!(cached_child.created_at, child.metadata.created_at);
        assert_eq!(
            cached_child.parent_session_id,
            child.metadata.parent_session_id
        );
        assert_eq!(
            app.session_title.as_deref(),
            Some(child.metadata.title.as_str())
        );
        drop(home_guard);
        assert_eq!(std::env::var_os("HOME"), previous_home);
    }

    #[test]
    fn fork_rejects_active_runtime_without_switching_sessions() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("parent-session".to_string());
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "still running".to_string(),
                cache_control: None,
            }],
        });
        app.is_loading = true;

        let result = fork(&mut app);

        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(app.current_session_id.as_deref(), Some("parent-session"));
        assert_eq!(app.api_messages.len(), 1);
    }

    #[test]
    fn new_session_from_resumed_state_creates_distinct_empty_session() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("old-session".to_string());
        app.session_title = Some("Old Session".to_string());
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "continue this thread".to_string(),
                cache_control: None,
            }],
        });
        app.add_message(HistoryCell::System {
            content: "old transcript".to_string(),
        });
        app.system_prompt = Some(crate::models::SystemPrompt::Text("old prompt".to_string()));
        app.session.total_tokens = 123;
        app.session.session_cost = 1.25;

        let result = new_session(&mut app, None);

        assert!(!result.is_error, "{:?}", result.message);
        let new_id = app.current_session_id.clone().expect("new session id");
        assert_ne!(new_id, "old-session");
        assert_eq!(app.session_title.as_deref(), Some("New Session"));
        assert!(app.api_messages.is_empty());
        assert!(app.history.is_empty());
        assert!(app.system_prompt.is_none());
        assert_eq!(app.session.total_tokens, 0);
        assert_eq!(app.session.session_cost, 0.0);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("/resume")
        );
        match result.action {
            Some(AppAction::SyncSession {
                session_id,
                messages,
                system_prompt,
                ..
            }) => {
                assert_eq!(session_id.as_deref(), Some(new_id.as_str()));
                assert!(messages.is_empty());
                assert!(system_prompt.is_none());
            }
            other => panic!("expected SyncSession action, got {other:?}"),
        }
    }

    #[test]
    fn new_session_blocks_unsent_input_without_force() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("old-session".to_string());
        app.input = "draft text".to_string();

        let result = new_session(&mut app, None);

        assert!(result.is_error);
        assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
        assert_eq!(app.input, "draft text");
        assert!(result.action.is_none());
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("/new --force")
        );
    }

    #[test]
    fn new_session_force_discards_unsent_input() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("old-session".to_string());
        app.input = "draft text".to_string();

        let result = new_session(&mut app, Some("--force"));

        assert!(!result.is_error, "{:?}", result.message);
        assert_ne!(app.current_session_id.as_deref(), Some("old-session"));
        assert!(app.input.is_empty());
        assert!(matches!(result.action, Some(AppAction::SyncSession { .. })));
    }

    #[test]
    fn new_session_blocks_in_flight_turn_without_force() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("old-session".to_string());
        app.is_loading = true;

        let result = new_session(&mut app, None);

        assert!(result.is_error);
        assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
        assert!(result.action.is_none());
    }

    #[test]
    fn new_session_force_cannot_detach_an_in_flight_turn() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("old-session".to_string());
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![],
        });
        app.is_loading = true;
        app.runtime_turn_status = Some("in_progress".to_string());

        let result = new_session(&mut app, Some("--force"));

        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
        assert_eq!(app.api_messages.len(), 1);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("only discards draft or queued input"))
        );
    }

    #[test]
    fn load_rejects_an_active_runtime_before_reading_or_mutating() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.current_session_id = Some("old-session".to_string());
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![],
        });
        app.task_panel.push(crate::tui::app::TaskPanelEntry {
            id: "queued-late-producer".to_string(),
            status: "queued".to_string(),
            prompt_summary: "queued".to_string(),
            duration_ms: None,
            kind: crate::tui::app::TaskPanelEntryKind::Background,
            stale: false,
            elapsed_since_output_ms: None,
            owner_agent_id: None,
            owner_agent_name: None,
        });

        let result = load(&mut app, Some("does-not-exist.json"));

        assert!(result.is_error);
        assert!(result.action.is_none());
        assert_eq!(app.current_session_id.as_deref(), Some("old-session"));
        assert_eq!(app.api_messages.len(), 1);
        assert!(
            result
                .message
                .as_deref()
                .is_some_and(|message| message.contains("runtime work is active"))
        );
    }

    #[test]
    fn test_save_with_default_path_uses_managed_sessions_dir() {
        let tmpdir = TempDir::new().unwrap();
        let _lock = crate::test_support::lock_test_env();
        // Set CODEWHALE_HOME so the managed sessions directory lands inside the
        // temp dir rather than the real user home. Pre-create the directory so
        // resolve_state_dir picks it up instead of falling back to legacy.
        let home = tmpdir.path().join("home");
        let sessions_dir = home.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &home);
        let previous_codewhale_home = codewhale_home.previous();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = save(&mut app, None);
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        // Give it a moment to ensure file is written
        std::thread::sleep(std::time::Duration::from_millis(10));
        let entries: Vec<_> = if sessions_dir.exists() {
            std::fs::read_dir(&sessions_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .collect()
        } else {
            Vec::new()
        };
        drop(codewhale_home);
        // Session should be saved to the managed dir, not the workspace root.
        assert!(
            !entries.is_empty(),
            "expected session file in {sessions_dir:?}, got none; msg: {msg}"
        );
        let session_id = app
            .current_session_id
            .as_deref()
            .expect("current session id");
        assert!(sessions_dir.join(format!("{session_id}.json")).exists());
        assert_eq!(std::env::var_os("CODEWHALE_HOME"), previous_codewhale_home);
    }

    #[test]
    fn test_save_serialization_error() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        // This should work normally since SavedSession is serializable
        // Testing error path would require mocking, which is complex
        let save_path = tmpdir.path().join("test.json");
        let result = save(&mut app, Some(save_path.to_str().unwrap()));
        assert!(result.message.is_some());
    }

    #[test]
    fn test_load_without_path_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = load(&mut app, None);
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("Usage: /load"));
    }

    #[test]
    fn test_load_nonexistent_file_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = load(&mut app, Some("nonexistent.json"));
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("Failed to read"));
    }

    #[test]
    fn test_load_invalid_json_returns_error() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let bad_file = tmpdir.path().join("bad.json");
        std::fs::write(&bad_file, "not valid json").unwrap();
        let result = load(&mut app, Some(bad_file.to_str().unwrap()));
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("Failed to parse"));
    }

    #[test]
    fn test_load_valid_session_defers_state_restore_to_event_loop() {
        let tmpdir = TempDir::new().unwrap();
        let mut app1 = create_test_app_with_tmpdir(&tmpdir);
        // Set up some state to save
        app1.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        });
        app1.session.total_tokens = 500;
        app1.set_mode(AppMode::Plan);
        let save_path = tmpdir.path().join("test.json");
        save(&mut app1, Some(save_path.to_str().unwrap()));

        // Create new app and load
        let mut app2 = create_test_app_with_tmpdir(&tmpdir);
        app2.system_prompt = Some(crate::models::SystemPrompt::Text(
            "stale prompt from prior session".to_string(),
        ));
        app2.session_context_references
            .push(crate::session_manager::SessionContextReference {
                message_index: 0,
                reference: crate::tui::file_mention::ContextReference {
                    kind: crate::tui::file_mention::ContextReferenceKind::File,
                    source: crate::tui::file_mention::ContextReferenceSource::AtMention,
                    badge: "file".to_string(),
                    label: "stale.rs".to_string(),
                    target: tmpdir.path().join("stale.rs").display().to_string(),
                    included: true,
                    expanded: true,
                    detail: None,
                },
            });
        let result = load(&mut app2, Some(save_path.to_str().unwrap()));
        assert_eq!(result.message, None);
        assert!(app2.api_messages.is_empty());
        assert_eq!(app2.session.total_tokens, 0);
        assert!(app2.current_session_id.is_none());
        assert!(app2.system_prompt.is_some());
        assert_eq!(app2.session_context_references.len(), 1);
        assert!(matches!(
            result.action,
            Some(AppAction::LoadSession(path)) if path == save_path
        ));
    }

    #[test]
    fn explicit_save_persists_work_state_and_load_defers_application() {
        let tmpdir = TempDir::new().unwrap();
        let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
        {
            let mut todos = saved_app.todos.try_lock().expect("todos lock");
            todos.add(
                "persist me".to_string(),
                crate::tools::todo::TodoStatus::InProgress,
            );
        }
        {
            let mut plan = saved_app.plan_state.try_lock().expect("plan lock");
            plan.update(crate::tools::plan::UpdatePlanArgs {
                objective: Some("Resume exactly".to_string()),
                ..crate::tools::plan::UpdatePlanArgs::default()
            });
        }
        let expected = saved_app.work_state_snapshot().expect("snapshot");
        let save_path = tmpdir.path().join("work_state.json");
        let saved = save(&mut saved_app, Some(save_path.to_str().unwrap()));
        assert!(!saved.is_error, "{:?}", saved.message);

        let mut loaded_app = create_test_app_with_tmpdir(&tmpdir);
        let loaded = load(&mut loaded_app, Some(save_path.to_str().unwrap()));
        assert!(!loaded.is_error, "{:?}", loaded.message);
        assert_eq!(loaded_app.work_state_snapshot().expect("snapshot"), None);
        assert!(matches!(
            loaded.action,
            Some(AppAction::LoadSession(path)) if path == save_path
        ));
        let saved_session: crate::session_manager::SavedSession =
            serde_json::from_str(&std::fs::read_to_string(&save_path).expect("saved session file"))
                .expect("saved session JSON");
        assert_eq!(saved_session.work_state, expected);
    }

    #[test]
    fn new_session_is_all_or_nothing_when_work_state_is_busy() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![],
        });
        app.current_session_id = Some("current-session".to_string());
        let todos = app.todos.clone();
        let _held = todos.try_lock().expect("hold todos lock");

        let result = new_session(&mut app, Some("--force"));

        assert!(result.is_error);
        assert_eq!(app.api_messages.len(), 1);
        assert_eq!(app.current_session_id.as_deref(), Some("current-session"));
        assert!(result.action.is_none());
    }

    #[test]
    fn load_auto_model_session_defers_model_restore_to_event_loop() {
        let tmpdir = TempDir::new().unwrap();
        let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
        saved_app.set_model_selection("auto".to_string());
        saved_app.last_effective_model = Some("deepseek-v4-flash".to_string());
        saved_app.last_effective_reasoning_effort = Some(ReasoningEffort::Low);
        let save_path = tmpdir.path().join("auto_model.json");
        save(&mut saved_app, Some(save_path.to_str().unwrap()));

        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.set_model_selection("deepseek-v4-flash".to_string());
        app.reasoning_effort = ReasoningEffort::High;
        let result = load(&mut app, Some(save_path.to_str().unwrap()));

        assert!(!result.is_error);
        assert!(!app.auto_model);
        assert_eq!(app.model, "deepseek-v4-flash");
        assert_eq!(app.reasoning_effort, ReasoningEffort::High);
        assert!(matches!(
            result.action,
            Some(AppAction::LoadSession(path)) if path == save_path
        ));
    }

    #[test]
    fn load_defers_artifact_registry_restore_to_event_loop() {
        let tmpdir = TempDir::new().unwrap();
        let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
        saved_app
            .session_artifacts
            .push(crate::artifacts::ArtifactRecord {
                id: "art_call_big".to_string(),
                kind: crate::artifacts::ArtifactKind::ToolOutput,
                session_id: "artifact-session".to_string(),
                tool_call_id: "call-big".to_string(),
                tool_name: "exec_shell".to_string(),
                success: Some(true),
                created_at: chrono::Utc::now(),
                byte_size: 128,
                preview: "checking crate".to_string(),
                storage_path: tmpdir.path().join("call-big.txt"),
            });
        let save_path = tmpdir.path().join("artifact_load.json");
        save(&mut saved_app, Some(save_path.to_str().unwrap()));

        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.session_artifacts
            .push(crate::artifacts::ArtifactRecord {
                id: "art_stale".to_string(),
                kind: crate::artifacts::ArtifactKind::ToolOutput,
                session_id: "stale-session".to_string(),
                tool_call_id: "stale".to_string(),
                tool_name: "exec_shell".to_string(),
                success: None,
                created_at: chrono::Utc::now(),
                byte_size: 1,
                preview: "stale".to_string(),
                storage_path: tmpdir.path().join("stale.txt"),
            });

        let result = load(&mut app, Some(save_path.to_str().unwrap()));

        assert!(!result.is_error);
        assert_eq!(app.session_artifacts.len(), 1);
        assert_eq!(app.session_artifacts[0].id, "art_stale");
        assert!(matches!(
            result.action,
            Some(AppAction::LoadSession(path)) if path == save_path
        ));
    }

    #[test]
    fn load_defers_telemetry_reset_to_event_loop() {
        let tmpdir = TempDir::new().unwrap();
        let mut saved_app = create_test_app_with_tmpdir(&tmpdir);
        saved_app.api_messages.push(crate::models::Message {
            role: "user".to_string(),
            content: vec![crate::models::ContentBlock::Text {
                text: "checkpoint".to_string(),
                cache_control: None,
            }],
        });
        saved_app.session.total_tokens = 500;
        let save_path = tmpdir.path().join("checkpoint.json");
        save(&mut saved_app, Some(save_path.to_str().unwrap()));

        let mut app = create_test_app_with_tmpdir(&tmpdir);
        app.session.session_cost = 1.25;
        app.session.session_cost_cny = 9.13;
        app.session.subagent_cost = 0.75;
        app.session.subagent_cost_cny = 5.48;
        app.session.subagent_cost_event_seqs.insert(42);
        app.session.displayed_cost_high_water = 2.0;
        app.session.displayed_cost_high_water_cny = 14.61;
        app.session.last_prompt_tokens = Some(120);
        app.session.last_completion_tokens = Some(35);
        app.session.last_prompt_cache_hit_tokens = Some(80);
        app.session.last_prompt_cache_miss_tokens = Some(40);
        app.session.last_reasoning_replay_tokens = Some(12);
        app.push_turn_cache_record(TurnCacheRecord {
            provider: None,
            provider_identity: None,
            model: None,
            auto_model: false,
            input_tokens: 120,
            output_tokens: 35,
            cache_hit_tokens: Some(80),
            cache_miss_tokens: Some(40),
            reasoning_replay_tokens: Some(12),
            recorded_at: Instant::now(),
        });

        let result = load(&mut app, Some(save_path.to_str().unwrap()));

        assert_eq!(result.message, None);
        assert_eq!(app.session.total_tokens, 0);
        assert_eq!(app.session.session_cost, 1.25);
        assert_eq!(app.session.session_cost_cny, 9.13);
        assert_eq!(app.session.subagent_cost, 0.75);
        assert_eq!(app.session.subagent_cost_cny, 5.48);
        assert_eq!(app.session.turn_cache_history.len(), 1);
        assert!(matches!(
            result.action,
            Some(AppAction::LoadSession(path)) if path == save_path
        ));
    }

    #[test]
    fn test_compact_toggles_state() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);

        let result = compact(&mut app);
        assert!(result.message.is_some());
        let msg = result.message.unwrap();
        assert!(msg.contains("compaction") || msg.contains("Compact"));
        assert!(matches!(result.action, Some(AppAction::CompactContext)));
    }

    #[test]
    fn test_sessions_pushes_picker_view() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let initial_kind = app.view_stack.top_kind();

        let result = sessions(&mut app, None);
        assert_eq!(result.message, None);
        assert!(result.action.is_none());
        // View should have changed (session picker should be on top)
        assert_ne!(app.view_stack.top_kind(), initial_kind);
    }

    #[test]
    fn test_sessions_show_subcommand_pushes_picker_view() {
        // `/sessions show` and `/sessions list` are explicit aliases
        // for the no-arg picker form. Verify they don't fall through
        // to the prune branch.
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let initial_kind = app.view_stack.top_kind();
        let result = sessions(&mut app, Some("show"));
        assert_eq!(result.message, None);
        assert_ne!(app.view_stack.top_kind(), initial_kind);
    }

    #[test]
    fn test_sessions_prune_requires_days_argument() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = sessions(&mut app, Some("prune"));
        assert!(result.is_error);
        assert!(
            result.message.as_deref().unwrap_or("").contains("usage"),
            "expected usage hint: {:?}",
            result.message
        );
    }

    #[test]
    fn test_sessions_prune_rejects_non_positive_days() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        for bad in ["0", "-3", "abc", "3.14"] {
            let result = sessions(&mut app, Some(&format!("prune {bad}")));
            assert!(result.is_error, "expected error for `{bad}`");
        }
    }

    #[test]
    fn test_sessions_unknown_subcommand_errors() {
        let tmpdir = TempDir::new().unwrap();
        let mut app = create_test_app_with_tmpdir(&tmpdir);
        let result = sessions(&mut app, Some("teleport"));
        assert!(result.is_error);
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or("")
                .contains("unknown subcommand"),
            "expected unknown-subcommand error: {:?}",
            result.message
        );
    }
}
