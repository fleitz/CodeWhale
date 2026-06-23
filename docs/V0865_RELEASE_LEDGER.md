# v0.8.65 Release Ledger

Updated: 2026-06-23 10:29 America/Los_Angeles.

This ledger tracks completion by concrete outcome: merged, replaced by a clean PR, absorbed with evidence, closed after implementation evidence, or blocked only by Hunter approval.

## Current Baseline

| Item | Track | Status | Completion evidence required | Next action |
| --- | --- | --- | --- | --- |
| PR #3468 WeCom `activeTurnId` fix | B/H | Merged into `main` | PR #3468 merged after green CI | Done |
| PR #3472 retired sub-agent refs | B | Merged into `main` | PR #3472 merged after green CI | Done |
| PR #3476 digest archive route | B | Merged into `main` | PR #3476 merged after green CI | Done |
| PR #3473 fact-drift CI gate | B | Replaced | Conflicting PR #3473 closed with evidence; clean replacement PR #3481 open and closes #3415 | Wait for #3481 CI, then merge if green |
| PR #3481 fact-drift CI gate replacement | B | Open, mergeable, CI queued | PR #3481 merged after green CI | `gh pr view 3481 --json mergeable,statusCheckRollup` |
| PR #3479 YOLO git tag probe fix | B/H | Open, mergeable, waiting CodeQL Rust | PR #3479 merged after all checks complete | `gh pr view 3479 --json mergeable,statusCheckRollup` |
| Issue #3477 install script | C | Local worktree only, no PR yet | PR closes #3477; `sh -n`, web lint/build, route smoke | Verify and commit `CodeWhale-install-script`, then open PR |
| Release ledger | A | Created locally in `CodeWhale-v0865-release-ledger` | This file committed and PR opened/merged if durable tracking is desired | Keep updated as lanes land |

## Worktree Layout

The outer `/Users/hunter/Desktop/Harnesses/CodeWhale` directory is a harness folder, not a Git repository. Keep unrelated repos there, but do not use them for CodeWhale release work.

CodeWhale worktrees currently in use:

| Worktree | Branch | Purpose |
| --- | --- | --- |
| `CodeWhale` | `milestone/v0.8.65-provider-model-routing` | Dirty provider-routing stabilization work; do not reset |
| `CodeWhale-install-script` | `codex/install-script-website` | Track C install script work |
| `CodeWhale-yolo-approval` | `codex/v0.8.65-yolo-git-readonly-approval` | PR #3479 |
| `CodeWhale-pr3473-fix` | `codex/finish-pr-3473` | Clean replacement PR #3481 |
| `CodeWhale-v0865-release-ledger` | `codex/v0.8.65-release-ledger` | This coordination ledger |

Unrelated repos under the same harness folder include `codew`, `codewhale-bench`, `codewhale-bench-v0862-final`, and `cw-deepswe`.

## Merge Order

1. Finish Phase 1 queue: #3479 and #3481 after CI is green.
2. Open and merge installer PR for #3477 after local verification.
3. Stabilize provider route resolution from clean worktrees before dependent provider UI/pricing/context work.
4. Land Fleet substrate/loadout/persona work before Fleet parity proof and final docs.
5. Run full release verification before any version bump, tag, artifact publish, or GitHub Release.

## Blockers

| Blocker | Track | Owner action |
| --- | --- | --- |
| #3479 CodeQL Rust still running | B/H | Wait; merge only after completion |
| #3481 CI queued | B | Wait; merge only after completion |
| #3477 branch uncommitted and unverified | C | Run installer verification, commit, open PR |
| Version bump/tag/release | A | Blocked on Hunter approval only |

## Phase 1 Commands

```bash
gh pr view 3479 --json mergeable,statusCheckRollup
gh pr view 3481 --json mergeable,statusCheckRollup
cd /Users/hunter/Desktop/Harnesses/CodeWhale/CodeWhale-install-script && git status --short --branch
```
