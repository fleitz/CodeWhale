# Train 5 Result — Composer / Steering / TUI Clarity

Branch: `codex/v0.8.61-train-5`

Final worktree status: clean (`git status --short --branch` -> `## codex/v0.8.61-train-5`).

Note: live `gh issue view` refresh was blocked by network access (`error connecting to api.github.com`), so implementation used the in-worktree triage docs plus code/tests.

## #3203 + #3224 — Queued Steering, Ctrl+S, Ctrl+Enter

Status: Done in `f72f07c2d fix(tui): clarify queued steering controls`.

Files:
- `crates/tui/src/commands/groups/core/mod.rs`
- `crates/tui/src/localization.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/tui/ui/tests.rs`
- `crates/tui/src/tui/widgets/pending_input_preview.rs`

Tests:
- `cargo fmt --all` -> pass
- `cargo test -p codewhale-tui --bin codewhale-tui pending_input_preview --locked` -> pass, 14 tests
- `cargo test -p codewhale-tui --bin codewhale-tui queue_send --locked` -> pass, 2 tests
- `cargo test -p codewhale-tui --bin codewhale-tui enter_while_model_waiting --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui composer_newline_shortcuts_do_not_steal_ctrl_enter --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui cmd_enter_normalizes_to_control_enter_not_newline --locked` -> pass, 1 test
- `git diff --check` -> pass

Risks: Ctrl+Enter behavior still depends on terminal key-event reporting; tests cover the local normalization and conflict path.

## #2054 — Queued-Steer Labels and Row Actions

Status: Done in `f72f07c2d fix(tui): clarify queued steering controls`.

Files:
- `crates/tui/src/localization.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/tui/ui/tests.rs`
- `crates/tui/src/tui/widgets/pending_input_preview.rs`

Tests:
- `cargo test -p codewhale-tui --bin codewhale-tui pending_input_preview --locked` -> pass, 14 tests
- `cargo test -p codewhale-tui --bin codewhale-tui queue_send --locked` -> pass, 2 tests

Risks: Row actions are exposed as clear slash commands (`/queue send <n>`, `drop <n>`, `clear`) rather than new mouse buttons in the composer preview.

## #2982 — Footer Busy/Idle Indicator

Status: Done in `79f069296 fix(tui): show footer busy idle state`.

Files:
- `crates/tui/src/config.rs`
- `crates/tui/src/tui/footer_ui.rs`
- `crates/tui/src/tui/ui/tests.rs`
- `crates/tui/src/tui/widgets/footer.rs`

Tests:
- `cargo fmt --all` -> pass
- `cargo test -p codewhale-tui --bin codewhale-tui footer_state_label --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui footer_status_line_spans --locked` -> pass, 2 tests
- `cargo test -p codewhale-tui --bin codewhale-tui from_app_loading_state_uses_busy_label --locked` -> pass, 1 test
- `git diff --check` -> pass

Risks: Indicator follows existing app liveness state; it does not introduce a separate provider-health state machine.

## #963 — Word-Wrap Truncation Fix

Status: Guarded in `de7e7c66e test(tui): pin cjk word wrap regression`.

Files:
- `crates/tui/src/tui/markdown_render.rs`

Tests:
- `cargo fmt --all` -> pass
- `cargo test -p codewhale-tui --bin codewhale-tui paragraph_wrap_breaks_no_whitespace_cjk_at_width_40 --locked` -> pass, 1 test
- `git diff --check` -> pass

Risks: This was a regression pin for the existing wrapping path, not a broad markdown renderer rewrite.

## #3028 + #3078 — Sidebar Click Actions, Stop Targets, Completed Sub-Agent TTL

Status: Done in `1c50d43d2 fix(tui): add sidebar stop targets`.

Files:
- `crates/tui/src/tui/app.rs`
- `crates/tui/src/tui/mouse_ui.rs`
- `crates/tui/src/tui/sidebar.rs`
- `crates/tui/src/tui/subagent_routing.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/tui/ui/tests.rs`

Tests:
- `cargo fmt --all` -> pass
- `cargo test -p codewhale-tui --bin codewhale-tui completed_subagent --locked` -> pass, 3 tests
- `cargo test -p codewhale-tui --bin codewhale-tui task_panel_actions --locked` -> pass, 3 tests
- `cargo test -p codewhale-tui --bin codewhale-tui sidebar_hover_rows_assign_stop_zone --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui sidebar_click_routes_inline_stop_zone --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui task_panel_finished_job_detail_row_shows_instead_of_cancels --locked` -> pass, 1 test
- `git diff --check` -> pass

Risks: `[x]` stop targeting is based on the rendered sidebar row columns. Completed sub-agent TTL clears cached/sidebar/view rows; transcript history remains intact.

## #3190 + #2666 — Token Throughput Telemetry

Status: Done in `1be50deb9 feat(tui): surface output token throughput`.

Files:
- `crates/tui/src/commands/groups/config/config.rs`
- `crates/tui/src/commands/groups/core/core.rs`
- `crates/tui/src/commands/groups/session/session.rs`
- `crates/tui/src/resource_telemetry.rs`
- `crates/tui/src/tui/app.rs`
- `crates/tui/src/tui/footer_ui.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/tui/ui/tests.rs`

Tests:
- `cargo fmt --all` -> pass
- `cargo test -p codewhale-tui resource_telemetry --locked` -> pass, 27 resource telemetry tests
- `cargo test -p codewhale-tui --bin codewhale-tui footer_session_tokens_chip --locked` -> pass, 3 tests
- `git diff --check` -> pass

Risks: Live streaming throughput is approximate from streamed text until provider usage arrives. The final post-turn rate uses provider-reported output tokens and elapsed turn duration.

## #3194 — Helper-Hint Audit

Status: Done in `1be50deb9 feat(tui): surface output token throughput`.

Files:
- `crates/tui/src/localization.rs`
- `crates/tui/src/tui/footer_ui.rs`
- `crates/tui/src/tui/key_shortcuts.rs`
- `crates/tui/src/tui/sidebar.rs`
- `crates/tui/src/tui/ui.rs`
- `crates/tui/src/tui/ui/tests.rs`

Tests:
- `cargo fmt --all` -> pass
- `cargo test -p codewhale-tui --bin codewhale-tui active_tool_status_label_summarizes_live_tool_group --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui activity_footer_hint_uses_details_for_subagent_cards --locked` -> pass, 1 test
- `cargo test -p codewhale-tui --bin codewhale-tui detail_target_prefers_visible_tool_card --locked` -> pass, 1 test
- `git diff --check` -> pass

Risks: Localized strings were updated directly to mention `Alt+V` plus the plain `v` fallback; no separate localization snapshot suite was present.

## Commits

- `f72f07c2d fix(tui): clarify queued steering controls`
- `79f069296 fix(tui): show footer busy idle state`
- `de7e7c66e test(tui): pin cjk word wrap regression`
- `1c50d43d2 fix(tui): add sidebar stop targets`
- `1be50deb9 feat(tui): surface output token throughput`

No PRs opened. No push performed. No tag, publish, release, version, Cargo.lock, CHANGELOG, or README version-file changes.
