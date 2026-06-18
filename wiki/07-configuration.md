# CodeWhale — Configuration Reference

> **Source:** `crates/config/src/lib.rs` (8080 lines), `config.example.toml`

---

## 1. Config File Location

CodeWhale reads configuration from (in priority order):
1. `--config` / `-c` CLI flag
2. `CODEWHALE_CONFIG` environment variable
3. `./config.toml` (current directory)
4. `~/.codewhale/config.toml` (user home)

Example shipped as `config.example.toml` in the repository root.

---

## 2. Top-Level Structure

```toml
# config.toml
[general]
model = "deepseek-v4-pro"        # default model
model_provider = "deepseek"       # default provider
workspace = "/path/to/project"    # optional default workspace

[runtime]
# ... runtime tuning

[subagents]
# ... sub-agent concurrency and policy

[providers]
# ... per-provider API keys and endpoints

[harness]
# ... harness posture and behavior

[fleet]
# ... headless worker configuration

[mcp]
# ... MCP server definitions

[hooks]
# ... hook sink configuration

[search]
# ... search backend selection
```

---

## 3. `[runtime]` — Runtime Tuning

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `thinking_budget` | string | `"512"` | Default thinking token budget (or `"off"`) |
| `max_spawn_depth` | integer | `3` | Recursion depth budget for sub-agents (clamped to ceiling) |
| `shell_timeout_ms` | integer | `120000` | Default shell command timeout |
| `auto_approve` | boolean | `false` | Auto-approve all tool calls (dangerous) |
| `sandbox` | string | — | Sandbox mode (`"docker"`, `"none"`) |
| `persist_extended_history` | boolean | `false` | Persist full conversation history to disk |

**Recursion depth constants** (compile-time, in `crates/config/src/lib.rs`):
- `DEFAULT_SPAWN_DEPTH = 3` — default recursion budget
- `MAX_SPAWN_DEPTH_CEILING = 3` — hard safety cap on all configured depth values

A worker at `spawn_depth = 0` may spawn while `spawn_depth + 1 <= max_spawn_depth`.
A depth of 3 affords 3 nested delegation levels below root.

---

## 4. `[subagents]` — Sub-Agent Policy

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `max_concurrent` | integer | `20` | Maximum concurrent sub-agents |
| `launch_concurrency` | integer | `20` | How many start simultaneously |
| `api_timeout_secs` | integer | `300` | Per-step LLM API timeout for sub-agents |
| `result_timeout_ms` | integer | `30000` | Timeout waiting for sub-agent results |

---

## 5. `[fleet]` — Headless Worker Configuration

```toml
[fleet]
[fleet.exec]
max_turns = 4294967295              # effectively unbounded
max_spawn_depth = 3                 # recursive child budget
allowed_tools = []                  # always allowed (empty = all)
disallowed_tools = []               # always disallowed
append_system_prompt = ""           # injected into every worker
output_format = "text"              # "text" | "stream-json"
```

---

## 6. `[harness]` — Harness Posture

Controls runtime strategy: context preloading, sub-agent posture, prompt-cache stability vs quick exploration.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `kind` | string | `"standard"` | `"standard"`, `"cache_heavy"`, or `"lean"` |
| `max_subagents` | integer | `0` | Max concurrent sub-agents (0 = runtime default: 20) |
| `prefer_codebase_search` | boolean | `false` | Prefer search-based context over always-on docs |
| `compaction_strategy` | string | varies | Compaction strategy per posture |

**Posture defaults:**

| Posture | max_subagents | prefer_search | compaction |
|---------|---------------|---------------|------------|
| `standard` | 0 (20) | false | default |
| `cache_heavy` | 10 | false | prefix-cache |
| `lean` | 20 | true | aggressive |

---

## 7. `[providers]` — Provider Configuration

25+ model providers supported. Each provider has:

```toml
[providers.deepseek]
api_key = "sk-..."                  # or env: DEEPSEEK_API_KEY
base_url = "https://api.deepseek.com"
models = ["deepseek-v4-pro", "deepseek-v4-flash"]

[providers.openai_compatible]
api_key = "sk-..."
base_url = "https://api.openai.com/v1"

[providers.nvidia_nim]
api_key = "nvapi-..."
base_url = "https://integrate.api.nvidia.com/v1"

[providers.openrouter]
api_key = "sk-or-..."
base_url = "https://openrouter.ai/api/v1"

[providers.zai]                    # Z.AI (GLM models)
api_key = "..."
base_url = "https://api.z.ai"

[providers.volcengine]             # Volcengine Ark (DeepSeek)
api_key = "..."
base_url = "https://ark.cn-beijing.volces.com/api/v3"

[providers.atlascloud]
api_key = "..."
base_url = "https://api.atlascloud.com/v1"

[providers.wanjie_ark]
api_key = "..."
base_url = "https://api.wanjie.ai/v1"

[providers.arcee]
api_key = "..."
base_url = "https://api.arcee.ai/v1"
```

Supported provider kinds: `deepseek`, `openai`, `anthropic`, `google`, `nvidia_nim`, `openrouter`, `zai`, `volcengine`, `atlascloud`, `wanjie_ark`, `arcee`, `ollama`, `vllm`, `lmstudio`, `groq`, `together`, `fireworks`, `replicate`, `deepinfra`, `mistral`, `cohere`, `xai`, `perplexity`, `qwen`, `zhipu`, `moonshot`, `minimax`, `tencent`, `baidu`, `stepfun`, `doubao`.

---

## 8. `[mcp]` — MCP Server Definitions

```toml
[mcp]
[[mcp.servers]]
name = "my-server"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path"]
enabled = true

[servers.my-server.filter]
allow = ["read_file", "write_file"]   # empty = allow all
deny = ["delete_file"]                 # deny takes precedence
```

Each MCP server is a process spawned by CodeWhale. Tools are discovered via the MCP protocol and registered in the tool registry with the server name as a prefix qualifier.

---

## 9. `[hooks]` — Event Hooks

```toml
[hooks]
[[hooks.sinks]]
type = "jsonl"                       # "stdout", "jsonl", "webhook", "unix_socket"
path = "~/.codewhale/hooks.jsonl"    # for jsonl type

[[hooks.sinks]]
type = "webhook"
url = "https://example.com/hooks"
secret = "whale-secret"
```

Hook events fired: `ResponseStart`, `ResponseDelta`, `ResponseEnd`, `ToolLifecycle`, `JobLifecycle`, `ApprovalLifecycle`, `GenericEventFrame`.

---

## 10. `[search]` — Search Backend

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `provider` | string | `"duckduckgo"` | `"duckduckgo"`, `"bing"`, `"tavily"`, `"bocha"`, `"metaso"`, `"baidu"`, `"volcengine"`, `"sofya"` |
| `base_url` | string | — | Custom endpoint for DuckDuckGo-compatible backends |
| `api_key` | string | — | API key for commercial search backends |

---

## 11. `[exec]` — Execution Policy

```toml
[exec]
# Approval mode for tool execution
approval_policy = "suggest"          # "auto" | "suggest" | "required"

# Pre-approved commands (skip approval even in suggest/required mode)
[[exec.pre_approved]]
command_prefix = "cargo test"
args_pattern = ".*"

# Always-ask commands
[[exec.always_ask]]
command_prefix = "rm -rf"

# Network policy
[[exec.network_policy]]
domain = "github.com"
action = "allow"
```

The execution policy engine uses a 3-layer priority system:
1. **Built-in** — hardcoded safety rules (lowest priority)
2. **Agent** — rules from the current agent/session
3. **User** — rules from config.toml (highest priority)

Each layer contains `ToolAskRule` entries with command-prefix matching and regex-based args patterns.

---

## 12. Model Resolution and Model Registry

The `codewhale-agent` crate maintains a built-in `ModelRegistry` with pre-populated model entries. Each entry has:
- Canonical provider `id`
- `provider` kind
- `aliases` (user-facing names, case-insensitive)
- `supports_tools` and `supports_reasoning` flags

The registry resolves user-requested model names through:
1. Exact ID match
2. Alias match (case-insensitive)
3. Provider prefix match
4. Fallback to default model

Model families: `DeepSeek`, `Anthropic`, `OpenAI`, `Google`, `Meta`, `Mistral`, `Qwen`, `Grok`, `Cohere`, `GptOss`, `Inferencer`.

---

## 13. Environment Variables

| Variable | Purpose |
|----------|---------|
| `DEEPSEEK_API_KEY` | DeepSeek API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `CODEWHALE_CONFIG` | Override config file path |
| `CODEWHALE_SUBAGENT_PERF_TRACE` | Set to `1` for sub-agent performance tracing |
| `CODEWHALE_OFFLINE` | Set to `1` to disable update checks |

---

## 14. CLI Runtime Overrides

Many config values can be overridden at the CLI:

```
codewhale run --model deepseek-v4-flash \
              --workspace /path/to/project \
              --auto-approve \
              --thinking off \
              --max-spawn-depth 5
```

CLI flags take highest priority, then env vars, then config file.
