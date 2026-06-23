# Claude Repository Guidance

Read `AGENTS.md` first. This file exists only as a compact compatibility layer
for Claude-based agents working in this repository.

## Current Lane

Do not use this file to determine the current branch, release, or milestone.
CodeWhale release lanes move quickly. Derive the current lane from:

- the user's current goal or handoff,
- `git status --short --branch`,
- `git branch --show-current`,
- live GitHub issues, milestones, and PRs,
- CI/check state on the relevant branch or PR.

If those sources disagree, trust live state and call out the mismatch.

## Core Rules

- Never commit directly to `main`.
- Do not tag, publish, create GitHub Releases, or push release artifacts without
  Hunter's explicit approval.
- Merge only when the current user goal or handoff authorizes landing the lane,
  or Hunter explicitly approves that PR/queue. If merge approval is ambiguous,
  ask before merging.
- Preserve unrelated dirty work.
- Keep each branch and PR narrowly reviewable.
- Inspect linked issues, PRs, comments, code, tests, and CI before claiming work
  is fixed or safe to close.
- Keep CodeWhale branding while preserving first-class DeepSeek provider/model
  support.
- Preserve contributor credit when harvesting or splitting community work.

## Workflow

For active issue or release work, follow `AGENTS.md`:

1. Refresh live state.
2. Check for existing PR coverage.
3. Create or switch to the correct focused branch.
4. Implement a coherent slice.
5. Run formatting and targeted tests.
6. Commit, push, and open a PR with goal, changes, verification, risks, and
   issue linkage. Use draft only while the branch is still being validated.
7. Revisit the PR after CI. Fix failures, mark verified branches ready for
   review, merge when the lane is authorized to land, and update or close linked
   issues only after verifying the landed commit.
8. Treat merged, verified closure, or documented blocker as the terminal state;
   do not treat an open PR as finished work.

Use scratch integration branches only for learning conflicts or coupling. Do
not ship scratch branches directly.
