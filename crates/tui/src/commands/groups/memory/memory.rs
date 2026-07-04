//! `/memory` slash command — inspect and edit the user memory file.
//!
//! When the user-memory feature is opted-in (`[memory] enabled = true` in
//! config or `DEEPSEEK_MEMORY=on` in the environment), `/memory` shows
//! the current memory file path and contents inline. Subcommands let the
//! user clear or open the file:
//!
//! - `/memory` — show path + content
//! - `/memory show` — alias for the no-arg form
//! - `/memory clear` — replace the file contents with an empty marker
//! - `/memory path` — show only the resolved path
//! - `/memory project` — show project-scoped memory for this workspace
//! - `/memory help` — show command-specific help and the resolved path
//!
//! Editor integration (`/memory edit`) is intentionally minimal: the
//! command prints a copy-pasteable shell line to open the file in the
//! user's `$VISUAL` / `$EDITOR`, since the in-process external editor
//! plumbing requires terminal teardown that the slash-command handler
//! doesn't have access to.

use std::fs;
use std::path::Path;

use crate::commands::CommandResult;
use crate::tui::app::App;

const MEMORY_USAGE: &str =
    "/memory [show|path|clear|edit|project|project-path|project-clear|status|help]";

fn memory_help(path: &Path, project_path: &Path) -> String {
    format!(
        "Inspect or manage persistent memory files.\n\n\
         Usage: {MEMORY_USAGE}\n\n\
         User path: {}\n\
         Project path: {}\n\n\
         Subcommands:\n\
           /memory                Show the user-memory path and contents\n\
           /memory show           Alias for the no-arg form\n\
           /memory path           Print just the user-memory path\n\
           /memory clear          Replace the user-memory file with an empty marker\n\
           /memory edit           Print the editor command for the user-memory file\n\
           /memory project        Show project memory for this workspace\n\
           /memory project-path   Print just the project-memory path\n\
           /memory project-clear  Replace the project-memory file with an empty marker\n\
           /memory status         Show both memory paths and file states\n\
           /memory help           Show this help\n\n\
         Quick capture: type `# foo` in the composer to append a timestamped\n\
         user-memory bullet without firing a turn. The `remember` tool can use\n\
         `scope: \"project\"` for workspace-local conventions.",
        path.display(),
        project_path.display(),
    )
}

fn read_memory_body(path: &Path, empty_hint: &str, missing_hint: &str) -> String {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => {
            format!("{}\n({empty_hint})", path.display())
        }
        Ok(text) => format!("{}\n\n{}", path.display(), text.trim_end()),
        Err(_) => format!("{}\n({missing_hint})", path.display()),
    }
}

fn memory(app: &mut App, arg: Option<&str>) -> CommandResult {
    if !app.use_memory {
        return CommandResult::error(
            "user memory is disabled. Enable with `[memory] enabled = true` in `~/.codewhale/config.toml` or `DEEPSEEK_MEMORY=on` in your environment, then restart the TUI.",
        );
    }

    let path = app.memory_path.clone();
    let project_path = crate::memory::project_memory_path(&path, &app.workspace);
    let sub = arg.unwrap_or("show").trim();

    match sub {
        "" | "show" => {
            let body = read_memory_body(
                &path,
                "empty — add via `# foo` from the composer or have the model use the `remember` tool",
                "file does not exist yet — add via `# foo` from the composer to create it",
            );
            CommandResult::message(body)
        }
        "path" => CommandResult::message(path.display().to_string()),
        "project" => {
            let body = read_memory_body(
                &project_path,
                "empty — have the model use `remember` with `scope: \"project\"`",
                "file does not exist yet — have the model use `remember` with `scope: \"project\"` to create it",
            );
            CommandResult::message(body)
        }
        "project-path" => CommandResult::message(project_path.display().to_string()),
        "clear" => match fs::write(&path, "") {
            Ok(()) => CommandResult::message(format!("memory cleared: {}", path.display())),
            Err(err) => CommandResult::error(format!("failed to clear {}: {err}", path.display())),
        },
        "project-clear" => match fs::write(&project_path, "") {
            Ok(()) => CommandResult::message(format!(
                "project memory cleared: {}",
                project_path.display()
            )),
            Err(err) => {
                CommandResult::error(format!("failed to clear {}: {err}", project_path.display()))
            }
        },
        "edit" => CommandResult::message(format!(
            "to edit your memory file, run:\n\n  ${{VISUAL:-${{EDITOR:-vi}}}} {}",
            path.display()
        )),
        "status" => {
            let user_state = if path.exists() { "present" } else { "missing" };
            let project_state = if project_path.exists() {
                "present"
            } else {
                "missing"
            };
            CommandResult::message(format!(
                "user memory: {} ({user_state})\nproject memory: {} ({project_state})",
                path.display(),
                project_path.display()
            ))
        }
        "help" => CommandResult::message(memory_help(&path, &project_path)),
        _ => CommandResult::error(format!(
            "unknown subcommand `{sub}`. Try `/memory help`.\n\n{}",
            memory_help(&path, &project_path)
        )),
    }
}

pub(in crate::commands) const COMMAND_INFO: crate::commands::traits::CommandInfo =
    crate::commands::traits::CommandInfo {
        name: "memory",
        aliases: &[],
        usage: MEMORY_USAGE,
        description_id: crate::localization::MessageId::CmdMemoryDescription,
    };

pub(in crate::commands) struct MemoryCmd;

impl crate::commands::traits::RegisterCommand for MemoryCmd {
    fn info() -> &'static crate::commands::traits::CommandInfo {
        &COMMAND_INFO
    }

    fn execute(
        app: &mut crate::tui::app::App,
        arg: Option<&str>,
    ) -> crate::commands::CommandResult {
        memory(app, arg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions};
    use tempfile::TempDir;

    fn create_test_app_with_memory(tmpdir: &TempDir, use_memory: bool) -> App {
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
            use_memory,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn memory_help_lists_subcommands_and_resolved_path() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("help"));
        let msg = result.message.expect("help should return text");
        assert!(msg.contains("Usage: /memory [show|path|clear|edit|project"));
        assert!(msg.contains("/memory edit"));
        assert!(msg.contains("/memory project"));
        assert!(msg.contains(app.memory_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn memory_unknown_subcommand_points_to_help() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("wat"));
        let msg = result
            .message
            .expect("unknown subcommand should return text");
        assert!(msg.contains("Try `/memory help`"));
        assert!(msg.contains("/memory clear"));
    }

    #[test]
    fn memory_disabled_returns_enablement_hint() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, false);
        let result = memory(&mut app, None);
        let msg = result.message.expect("disabled memory should return text");
        assert!(msg.contains("user memory is disabled"));
        assert!(msg.contains("DEEPSEEK_MEMORY=on"));
    }

    #[test]
    fn memory_project_subcommand_reads_project_scoped_file() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let project_path = crate::memory::project_memory_path(&app.memory_path, &app.workspace);
        crate::memory::append_project_entry(
            &app.memory_path,
            &app.workspace,
            "this workspace runs cargo fmt",
        )
        .expect("append project memory");

        let result = memory(&mut app, Some("project"));
        let msg = result.message.expect("project memory should return text");
        assert!(msg.contains(project_path.to_string_lossy().as_ref()));
        assert!(msg.contains("cargo fmt"));
    }

    #[test]
    fn memory_status_reports_user_and_project_paths() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let project_path = crate::memory::project_memory_path(&app.memory_path, &app.workspace);

        let result = memory(&mut app, Some("status"));
        let msg = result.message.expect("status should return text");
        assert!(msg.contains(app.memory_path.to_string_lossy().as_ref()));
        assert!(msg.contains(project_path.to_string_lossy().as_ref()));
    }
}
