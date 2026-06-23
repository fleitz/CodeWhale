# v0.8.65 Release Ledger

Updated: 2026-06-23 10:35 America/Los_Angeles.

This ledger tracks completion by concrete outcome: merged, replaced by a clean PR, absorbed with evidence, closed after implementation evidence, or blocked only by Hunter approval.

## Current Baseline

| Item | Track | Status | Completion evidence required | Next action |
| --- | --- | --- | --- | --- |
| PR #3468 WeCom `activeTurnId` fix | B/H | Merged into `main` | PR #3468 merged after green CI | Done |
| PR #3472 retired sub-agent refs | B | Merged into `main` | PR #3472 merged after green CI | Done |
| PR #3476 digest archive route | B | Merged into `main` | PR #3476 merged after green CI | Done |
| PR #3473 fact-drift CI gate | B | Replaced | Conflicting PR #3473 closed with evidence; clean replacement PR #3481 open and closes #3415 | Wait for #3481 CI, then merge if green |
| PR #3481 fact-drift CI gate replacement | B | Open, mergeable, CI queued | PR #3481 merged after green CI | `gh pr view 3481 --json mergeable,statusCheckRollup` |
| PR #3479 YOLO git tag probe fix | B/H | Merged into `main` | PR #3479 merged after green CI | Done |
| Issue #3477 install script | C | Replaced by PR #3482 | PR #3482 closes #3477; `sh -n`, web lint/build, fake-release smoke completed locally | Wait for #3482 CI, then merge if green |
| PR #3482 install script | C | Open, mergeable, CI starting | PR #3482 merged after green CI | `gh pr view 3482 --json mergeable,statusCheckRollup` |
| Release ledger | A | Created locally in `CodeWhale-v0865-release-ledger` | This file committed and PR opened/merged if durable tracking is desired | Keep updated as lanes land |

## Worktree Layout

The outer `/Users/hunter/Desktop/Harnesses/CodeWhale` directory is a harness folder, not a Git repository. Keep unrelated repos there, but do not use them for CodeWhale release work.

CodeWhale worktrees currently in use:

| Worktree | Branch | Purpose |
| --- | --- | --- |
| `CodeWhale` | `milestone/v0.8.65-provider-model-routing` | Dirty provider-routing stabilization work; do not reset |
| `CodeWhale-install-script` | `codex/install-script-website` | Track C install script work |
| `CodeWhale-yolo-approval` | `codex/v0.8.65-yolo-git-readonly-approval` | PR #3479, merged; local worktree kept intact |
| `CodeWhale-pr3473-fix` | `codex/finish-pr-3473` | Clean replacement PR #3481 |
| `CodeWhale-v0865-release-ledger` | `codex/v0.8.65-release-ledger` | This coordination ledger |

Unrelated repos under the same harness folder include `codew`, `codewhale-bench`, `codewhale-bench-v0862-final`, and `cw-deepswe`.

## Merge Order

1. Finish Phase 1 queue: #3481 after CI is green.
2. Merge installer PR #3482 after CI is green.
3. Stabilize provider route resolution from clean worktrees before dependent provider UI/pricing/context work.
4. Land Fleet substrate/loadout/persona work before Fleet parity proof and final docs.
5. Run full release verification before any version bump, tag, artifact publish, or GitHub Release.

## Blockers

| Blocker | Track | Owner action |
| --- | --- | --- |
| #3481 Rust CodeQL, macOS, and Windows checks still running | B | Wait; merge only after completion |
| #3482 CI just started | C | Wait; merge only after completion |
| Version bump/tag/release | A | Blocked on Hunter approval only |

## Phase 1 Commands

```bash
gh pr view 3481 --json mergeable,statusCheckRollup
gh pr view 3482 --json mergeable,statusCheckRollup
```
