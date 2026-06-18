# CodeWhale Skills System

> **Version:** v0.8.62
> **Source:** `crates/tui/src/tools/skill.rs` (431 lines), `crates/tui/src/skills/mod.rs` (1883 lines), `crates/tui/src/skills/system.rs` (430 lines), `crates/tui/src/skills/install.rs` (1718 lines)

---

## Part 1: Concept

A **skill** is a set of domain-specific model instructions stored in a local `SKILL.md` file. Skills operate on a **progressive-disclosure** contract: the model sees a compact catalogue (name + description + file path) at the start of every turn, but the full body is loaded only when the task clearly matches that skill.

Skills are:

- **Static instructions** — they are Markdown files on disk, not live processes or APIs.
- **Domain-scoped** — each skill covers one workflow or tool category (e.g., PDF editing, delegation, spreadsheets).
- **Local-first** — skills live in workspace or global directories; no network request is needed to load them.
- **Companion-aware** — a skill directory can ship scripts, templates, or reference files alongside `SKILL.md`.

### Progressive Disclosure

The model budget is finite. At the start of every turn, the system prompt injects a one-line catalogue listing for every available skill — capped at ~12,000 characters and 280 characters per description (`mod.rs:24-25`). When a skill is relevant, the model opens it with `load_skill` (one call, returns the body plus companion-file listing) or with `read_file` (two-call dance: read `SKILL.md` then `list_dir`).

```
System prompt catalogue (every turn)   →  One-line per skill
load_skill / read_file (on demand)     →  Full body + companion listing
```

---

## Part 2: SKILL.md Format

A `SKILL.md` file lives inside a named directory. The only required file is `SKILL.md` itself:

```
my-skill/
└── SKILL.md
```

### Frontmatter (preferred)

Skills use YAML frontmatter delimited by `---` fences. The parser in `mod.rs:243-427` handles plain key-value pairs, quoted strings, and YAML block scalars (`>`, `|`, with `-`/`+` chomping).

```markdown
---
name: my-skill
description: Use when the user wants to do something specific.
metadata:
  short-description: Optional shorter label for constrained displays.
---

# My Skill

Instructions for the agent...
```

**Required field:** `name`
**Optional fields:** `description`, `metadata.short-description`

### Fallback: heading-based names

If no `---` fence is found, the parser extracts the first `# Heading` as the skill name (`mod.rs:409-427`). The description stays empty. This graceful-degradation path accepts plain Markdown files that don't follow the frontmatter convention.

### Block scalars

Multi-line descriptions are supported via YAML block scalar notation:

```yaml
description: >
  This is a folded description that
  becomes a single paragraph.
```

Supported indicators: `>`, `|`, `>-`, `>+`, `|-`, `|+` — each triggers block-scalar parsing with the correct chomping behaviour (`mod.rs:274-372`).

### Companion files

A skill directory may contain additional files alongside `SKILL.md`:

```
my-skill/
├── SKILL.md
├── script.py
├── data.json
└── references/
    └── api-docs.md      ← skipped (nested directory)
```

The `collect_companion_files` function (`skill.rs:189-211`) lists only **immediate sibling files** (excluding `SKILL.md` and nested directories). These paths appear under a `## Companion files` heading in the `load_skill` output, so the model can open them with `read_file` as needed.

---

## Part 3: Discovery System

### Discovery modes

Two modes control which directories are scanned (`mod.rs:46-65`):

| Mode | Behaviour |
|---|---|
| `Compatible` | Scan 10 directory roots across CodeWhale, agentskills.io, Claude, OpenCode, Cursor, and legacy DeepSeek conventions |
| `CodeWhaleOnly` | Scan only CodeWhale-owned roots (`.codewhale/skills` workspace + `~/.codewhale/skills` global) |

The mode is set via `SkillDiscoveryMode::from_codewhale_only(bool)` (`mod.rs:58-65`), driven by the `skills_scan_codewhale_only` configuration flag.

### Directory precedence (Compatible mode)

Skills are discovered by walking **10 candidate directory roots** in precedence order (`mod.rs:457-525`). First match wins on name conflicts — a workspace skill shadows a global skill with the same name.

| Precedence | Path | Scope | Convention |
|---|---|---|---|
| 1 | `<workspace>/.agents/skills` | Workspace | CodeWhale native |
| 2 | `<workspace>/skills` | Workspace | Flat, project-local |
| 3 | `<workspace>/.opencode/skills` | Workspace | OpenCode interop |
| 4 | `<workspace>/.claude/skills` | Workspace | Claude Code interop |
| 5 | `<workspace>/.cursor/skills` | Workspace | Cursor interop |
| 6 | `<workspace>/.codewhale/skills` | Workspace | CodeWhale workspace |
| 7 | `~/.agents/skills` | Global | agentskills.io |
| 8 | `~/.claude/skills` | Global | Claude ecosystem |
| 9 | `~/.codewhale/skills` | Global | **Primary install target** |
| 10 | `~/.deepseek/skills` | Global | Legacy DeepSeek fallback |

### CodeWhaleOnly mode

When `skills_scan_codewhale_only` is `true`, only two roots are scanned (`mod.rs:505-507, 517-519`):

| Precedence | Path |
|---|---|
| 1 | `<workspace>/.codewhale/skills` |
| 2 | `~/.codewhale/skills` |

Additionally, a configured `skills_dir` is always included regardless of mode — user configuration cannot be buried by the scan scope (`mod.rs:614-628`).

### Recursive walk

`SkillRegistry::discover(dir)` walks recursively with these rules (`mod.rs:114-231`):

- **Max depth**: 8 levels (`MAX_DISCOVERY_DEPTH`) — defends against pathological configurations.
- **Hidden directories skipped**: subdirectories starting with `.` (e.g., `.git/`, `.cache/`) are filtered.
- **Symlinks followed**: with canonical path tracking to prevent infinite loops.
- **Skill directory consumed**: when a `SKILL.md` is found inside a directory, that directory is marked as a skill and the walk does **not** descend further — nested subdirectories inside a skill are companion resources, not separately-installable skills.
- **Nested vendor layouts**: the recursive walk supports `<root>/<vendor>/<skill>/SKILL.md` layouts.
- **Warnings accumulated**: parse failures and I/O errors are collected and surfaced.

### On-disk layout on this system

The 12 shipped skills are installed under `~/.codewhale/skills/` by the system installer. Each is a solo `SKILL.md` file:

```
~/.codewhale/skills/
├── .system-installed-version          ← version marker ("4")
├── delegate/SKILL.md
├── documents/SKILL.md
├── feishu/SKILL.md
├── fleet-manager/SKILL.md
├── mcp-builder/SKILL.md
├── pdf/SKILL.md
├── plugin-creator/SKILL.md
├── presentations/SKILL.md
├── skill-creator/SKILL.md
├── skill-installer/SKILL.md
├── spreadsheets/SKILL.md
└── v4-best-practices/SKILL.md
```

---

## Part 4: System Skill Installation

Bundled first-party skills are auto-installed by `system.rs` on first launch.

### How it works

`system.rs:27-88` defines 12 `BundledSkill` entries, each with a `name`, `body` (compiled-in via `include_str!`), and `introduced_in` (bundle version when it appeared). The bodies are embedded into the binary at compile time.

`install_system_skills(skills_dir)` (`system.rs:146-163`) checks a version marker (`.system-installed-version`) and installs/updates skills:

- **Fresh install**: no marker, no directory → install all 12.
- **Version bump**: marker present with older version → re-install existing bundled skills, add newly introduced ones.
- **User-deleted skill dir**: marker present at current version → leaves it gone (respects user intent).
- **Idempotent**: calling twice with no changes is a no-op.

### Version history

| Bundle Version | Skills Introduced |
|---|---|
| 1 | `skill-creator` |
| 2 | `delegate` |
| 3 | `v4-best-practices`, `plugin-creator`, `skill-installer`, `mcp-builder`, `documents`, `presentations`, `spreadsheets`, `pdf`, `feishu` |
| 4 | `fleet-manager` |

The current version is `"4"` (`system.rs:7`).

### Community skill installer

`install.rs` (1718 lines) handles community-authored skills from GitHub repos, direct tarball URLs, or a curated registry (`DEFAULT_REGISTRY_URL: "https://raw.githubusercontent.com/Hmbown/deepseek-skills/main/index.json"`). It enforces path-traversal protection, a 5 MiB size cap, and atomic temp-directory extraction — half-installed skills can never appear on disk.

---

## Part 5: The `load_skill` Tool

### Tool spec

Defined in `skill.rs:40-154`.

```json
{
  "name": "load_skill",
  "description": "Load a skill (SKILL.md body + companion file list) into the next turn's context. Use this when the user names a skill or the task clearly matches a skill listed in the system prompt's `## Skills` section. Faster than read_file + list_dir.",
  "parameters": {
    "type": "object",
    "properties": {
      "name": {
        "type": "string",
        "description": "Skill id (the `name` field from the SKILL.md frontmatter, also shown in the `## Skills` listing)."
      }
    },
    "required": ["name"],
    "additionalProperties": false
  }
}
```

| Property | Value |
|---|---|
| Capabilities | `ReadOnly` |
| Approval | `Auto` (no user approval needed) |
| Parallel | `true` (can run alongside other tool calls) |

### Execution flow

`execute()` at `skill.rs:79-153`:

1. Validates that `name` is a non-empty string.
2. Determines the discovery mode from `context.skills_scan_codewhale_only`.
3. Builds a `SkillRegistry` by scanning all candidate directories (mirroring what the system-prompt skills block already lists).
4. Looks up the skill by name — returns a helpful error listing available skills (or installation instructions if none are found).
5. Formats the skill body with `format_skill_body()` and returns it with metadata.

### Response format

`format_skill_body()` at `skill.rs:161-183` produces:

```
# Skill: <name>

> <description>

Source: `<path>`

## SKILL.md

<full body>

## Companion files      ← only when companion files exist

- `<path/to/script.py>`
- `<path/to/data.json>`
```

The response also includes metadata (`skill.rs:145-153`):

```json
{
  "skill_name": "<name>",
  "skill_path": "<absolute path to SKILL.md>",
  "companion_files": ["<path1>", "<path2>"]
}
```

### Error messages

- **Unknown skill with alternatives**: `"skill 'imaginary' not found. Available: delegate, documents, feishu, ..."`
- **No skills installed**: `"no skills installed. Searched: <dir paths>"` plus installation instructions.
- **No directories exist**: `"no skills directories found; install skills under <workspace>/.codewhale/skills/<name>/SKILL.md or ~/.codewhale/skills/<name>/SKILL.md"`

---

## Part 6: System Prompt Injection

The system prompt injects a skills catalogue block via `prompts.rs:1109-1117`:

```rust
let skills_block = match skills_dir {
    Some(dir) => {
        crate::skills::render_available_skills_context_for_workspace_and_dir(workspace, dir)
    }
    None => crate::skills::render_available_skills_context_for_workspace(workspace),
};
if let Some(block) = skills_block {
    full_prompt = format!("{full_prompt}\n\n{block}");
}
```

### Injected block structure

`render_skills_block()` at `mod.rs:739-803` produces:

```markdown
## Skills
A skill is a set of local instructions stored in a `SKILL.md` file. Below is the list of skills available in this session. Each entry includes a name, description, and file path so you can open the source for full instructions when using a specific skill.

### Available skills
- delegate: Strategic delegation for multi-step coding, research, or verification work... (file: ~/.codewhale/skills/delegate/SKILL.md)
- documents: Create, edit, inspect, or convert Word documents and DOCX deliverables... (file: ~/.codewhale/skills/documents/SKILL.md)
...

### How to use skills
- Skill bodies live on disk at the listed paths. When a skill is relevant, open only that skill's `SKILL.md` and the specific companion files it references.
- Trigger rules: use a skill when the user names it (`$SkillName`, `/skill <name>`, or plain text) or the task clearly matches its description. Do not carry skills across turns unless re-mentioned.
- Missing/blocked: if a named skill is missing or cannot be read, say so briefly and continue with the best fallback.
- Safety: do not execute scripts from a community skill unless the user explicitly asks or the skill has been trusted for script use.
```

### Budget constraints

- **Max per-description**: 280 characters (`MAX_SKILL_DESCRIPTION_CHARS`, `mod.rs:24`) — longer descriptions are whitespace-collapsed and truncated with `…`.
- **Max total block**: 12,000 characters (`MAX_AVAILABLE_SKILLS_CHARS`, `mod.rs:25`) — skills beyond the budget are counted with `"... N additional skills omitted from this prompt budget."`
- **Max warnings**: 8 (`mod.rs:787`) — each truncated to 280 characters.
- **Empty suppression**: when no skills are discovered, the block is `None` and not injected at all.

---

## Part 7: The 12-Skill Catalogue

| # | Name | Description | Introduced | Key Instructions |
|---|---|---|---|---|
| 1 | **delegate** | Strategic delegation for multi-step coding, research, or verification work through sub-agents | v2 | Keep vs delegate decisions; agent open/eval/close patterns; prompt shape rules; verification of sub-agent claims |
| 2 | **documents** | Create, edit, inspect, or convert Word documents and DOCX deliverables | v3 | python-docx and pandoc workflows; preservation of originals; structure recommendations; verification steps |
| 3 | **feishu** | Feishu/Lark bots, docs, sheets, bitables, approval flows, and OpenAPI/MCP setup | v3 | China vs international API endpoints; credential handling via env vars; webhook and token flows; MCP server patterns |
| 4 | **fleet-manager** | Triage, restart, escalate, or summarize CodeWhale Agent Fleet runs and workers | v4 | Triage loop (6 states); restart-vs-escalate criteria; safe escalation draft template; post-run receipt format |
| 5 | **mcp-builder** | Design, build, configure, or debug Model Context Protocol servers | v3 | Stdio vs HTTP/SSE transport; tool schema design; credential handling; `deepseek mcp` commands |
| 6 | **pdf** | Read, extract, split, merge, rotate, watermark, fill, OCR, or create PDF files | v3 | Tool selection (pdftotext, qpdf, mutool, python libs); page coverage reporting; OCR quality caveats; redaction verification |
| 7 | **plugin-creator** | Scaffold local plugin directories and activation notes | v3 | Plugin layout (PLUGIN.md + skills/scripts/mcp/assets/); naming conventions; activation section; honesty about no auto-loader |
| 8 | **presentations** | Create, edit, inspect, or convert PowerPoint decks and PPTX files | v3 | python-pptx and LibreOffice workflows; outline-first approach; editable native elements over flattened screenshots; verification |
| 9 | **skill-creator** | Create or improve skills; guidance on skill vs MCP vs hooks vs plugin | v1 | Discovery path reference; minimum SKILL.md shape; writing rules; creation workflow; validation checklist |
| 10 | **skill-installer** | Install, update, trust, or inspect skills from GitHub or local folders | v3 | `/skill` commands; source identification; trust review before execution; conflict resolution (workspace over global) |
| 11 | **spreadsheets** | Create, edit, analyze, clean, or convert XLSX, CSV, TSV, and tabular data | v3 | Tool selection (openpyxl, pandas, csv); formula vs fixed-value decision; verification checklist; ID/date safety |
| 12 | **v4-best-practices** | Rules for multi-step V4 thinking-mode workflows to prevent stale references and unverified assumptions | v3 | Three rules: verify references with grep_files before writing; spawn verifier sub-agent before multi-file execution; plan output must use path:line references |

### Descriptions used in the system prompt catalogue

Each skill's `description` frontmatter field is the "trigger signal" the model sees in the catalogue. The shipped descriptions are:

| Name | Description (as seen in catalogue) |
|---|---|
| delegate | Strategic delegation for multi-step coding, research, or verification work. Use when a task can be split into parent reasoning plus focused sub-agent execution through agent_open, agent_eval, and agent_close. |
| documents | Create, edit, inspect, or convert Word documents and DOCX deliverables such as memos, reports, letters, templates, and forms. |
| feishu | Work with Feishu or Lark bots, docs, sheets, bitables, approval flows, and OpenAPI/MCP setup without hardcoding credentials. |
| fleet-manager | Use when managing, triaging, restarting, escalating, or summarizing CodeWhale Agent Fleet runs and workers. |
| mcp-builder | Design, build, configure, or debug Model Context Protocol servers for codewhale, including stdio and HTTP/SSE transports. |
| pdf | Read, extract, split, merge, rotate, watermark, fill, OCR, or create PDF files with verification of page counts and text extraction. |
| plugin-creator | Scaffold codewhale local plugin directories and activation notes. Use when the user asks to create, package, or sketch a plugin for codewhale. |
| presentations | Create, edit, inspect, or convert PowerPoint decks and PPTX slide presentations with practical layout and verification steps. |
| skill-creator | Create or improve codewhale skills. Use when the user wants a new skill, wants to update an existing skill, or needs guidance on when a skill should be a skill versus MCP, hooks, tools, or a plugin scaffold. |
| skill-installer | Install, update, trust, or inspect DeepSeek skills from GitHub or local skill folders. Use when the user asks for available skills or wants a community skill installed. |
| spreadsheets | Create, edit, analyze, clean, or convert spreadsheets including XLSX, CSV, TSV, formulas, charts, and tabular reports. |
| v4-best-practices | Use when working with deepseek-v4-pro or deepseek-v4-flash in thinking mode on multi-step or plan-driven tasks. Provides rules to prevent stale references, unverified plan assumptions, and vague plan output. |

---

## Part 8: Skills vs MCP vs Plugins vs Hooks

This comparison is drawn from the `skill-creator` skill body and the codebase architecture.

| Dimension | **Skills** | **MCP Servers** | **Plugins** | **Hooks** |
|---|---|---|---|---|
| **What it is** | Static Markdown instructions | Live process providing tools over stdio or HTTP/SSE | Packaging convention (PLUGIN.md + optional companion folders) | Event-driven local callbacks |
| **Loaded when** | Model requests via `load_skill` or `read_file` | Registered in `~/.deepseek/mcp.json`, launched on session start | Not auto-loaded; referenced by skills or MCP servers | Triggered by specific events (e.g., pre-tool, post-tool) |
| **Runtime** | No runtime — just text in context | Persistent child process with JSON-RPC | None (scaffold only; no plugin loader exists yet) | Runs in-process during tool execution |
| **Primary use** | Domain instructions, workflows, conventions | External APIs, durable tools, live services | Multi-piece packaging (skill + scripts + MCP + assets) | Automatic local events |
| **Network** | No network access | May connect to external services | No network access | No network access |
| **Install location** | `SKILL.md` in any of 10 discovery directories | Entry in `~/.deepseek/mcp.json` | `~/.deepseek/plugins/<name>/` or `<workspace>/plugins/<name>/` | Configured in config or tool registry |
| **Activation** | Name-based trigger or description match in catalogue | Always-on once configured (tools appear in tool list) | Must be wired through a skill, MCP, or hook reference | Event-driven (specific lifecycle points) |
| **Safety model** | No executable code — reading is safe; scripts require trust marker | Process sandboxing; tool invocation gated by approval | Scaffold only — activating requires explicit user wiring | In-process — must not be user-controllable |

**Decision heuristic from `skill-creator`:**

- **Instructional workflow** → skill
- **External service / live API** → MCP server + optional companion skill
- **Repeated shell helper** → local tool or script + optional companion skill
- **Packaging multiple pieces** → plugin scaffold + skill/MCP activation notes

---

## Part 9: Source File Reference

| File | Lines | Purpose |
|---|---|---|
| `crates/tui/src/tools/skill.rs` | 1–431 | `load_skill` tool: schema, execution, body formatting, companion-file collection, 6 tests |
| `crates/tui/src/skills/mod.rs` | 1–1883 | Skill discovery: `Skill`, `SkillRegistry`, recursive walk, directory precedence, discovery modes, system-prompt block rendering, catalogue truncation |
| `crates/tui/src/skills/system.rs` | 1–430 | System skill installer: 12 `BundledSkill` entries, version marker, install/update/uninstall logic, tests |
| `crates/tui/src/skills/install.rs` | 1–1718 | Community skill installer: source parsing (GitHub/Direct/Registry), tarball extraction with path-traversal protection, atomic install, sync, trust markers |
| `crates/tui/src/prompts.rs` | ~1109–1117 | System prompt injection point: calls `render_available_skills_context_for_workspace_and_dir` or `render_available_skills_context_for_workspace` |
| `crates/tui/src/context_report.rs` | ~303–305 | Context report: same skills-block injection for status display |
| `~/.codewhale/skills/` | 12 dirs | On-disk location of shipped system skills (each contains `SKILL.md`) |

### Key constants

| Constant | Value | Location |
|---|---|---|
| `MAX_SKILL_DESCRIPTION_CHARS` | 280 | `mod.rs:24` |
| `MAX_AVAILABLE_SKILLS_CHARS` | 12,000 | `mod.rs:25` |
| `MAX_DISCOVERY_DEPTH` | 8 | `mod.rs:93` |
| `BUNDLED_SKILL_VERSION` | `"4"` | `system.rs:7` |
| `DEFAULT_MAX_SIZE_BYTES` | 5 MiB | `install.rs:76` |
| `DEFAULT_REGISTRY_URL` | `https://raw.githubusercontent.com/Hmbown/deepseek-skills/main/index.json` | `install.rs:71-72` |
| `INSTALLED_FROM_MARKER` | `.installed-from` | `install.rs:81` |
| `TRUSTED_MARKER` | `.trusted` | `install.rs:86` |

---

## Part 10: End-to-End Flow

```
Startup
  │
  ├─ system.rs: install_system_skills(~/.codewhale/skills)
  │   └─ Checks .system-installed-version marker
  │   └─ Installs/updates any bundled skill whose version changed
  │
  └─ prompts.rs: builds system prompt
      └─ discover_in_workspace(workspace, mode)
          └─ skills_directories_for_mode → 6 workspace + 4 global paths (Compatible)
          └─ For each existing dir: SkillRegistry::discover(dir)
              └─ Recursive walk up to depth 8
              └─ Parse SKILL.md (frontmatter or heading fallback)
              └─ First-name-match-wins across directories
      └─ render_skills_block(registry)
          └─ "## Skills" header + catalogue lines + usage instructions
          └─ Truncated to 12,000 chars / 280 chars per description

Every turn
  │
  └─ System prompt contains ## Skills catalogue (one line per skill)

On demand
  │
  ├─ load_skill(name="pdf")
  │   └─ Same discovery scan as system prompt
  │   └─ Returns SKILL.md body + companion file list + metadata
  │
  └─ read_file(skill_path) + list_dir(skill_dir)
      └─ Alternative two-call path (always available)
```
