# Web Layer

CodeWhale's web layer spans five surfaces: a Next.js community site, a Tauri desktop/mobile app, three npm packages, the HTTP REST API, and a local-first device pairing mechanism (CodeWhale Link).

---

## 1. Next.js Community Site (`web/`)

**Purpose:** Public-facing community site at **[codewhale.net](https://codewhale.net)** — landing page, documentation, roadmap, install instructions, FAQ, contributor hub, and a live roadmap feed.

**Tech stack:**

| Layer | Choice |
|---|---|
| Framework | Next.js 15 (App Router, React 19, RSC) |
| Styling | Tailwind CSS 3, PostCSS |
| Language | TypeScript 5.7 |
| Fonts | Fraunces (display), IBM Plex Sans (body), JetBrains Mono (code), Noto Serif SC (CJK decorative) |
| Hosting | Cloudflare Workers via **OpenNext** (`@opennextjs/cloudflare`) |
| Data | Cloudflare KV (key-value store for GitHub stats, curated dispatch, facts) |
| CI/CD | `.github/workflows/web.yml` — lint → deploy on `workflow_dispatch` |
| Testing | Vitest |
| Linting | ESLint (flat config) |

**Key architecture decisions:**

- **Middleware** (`middleware.ts`): Locale detection (English `en` / Chinese `zh`) via cookie → Accept-Language header → default `en`. Applies security headers globally (`X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy`, `Permissions-Policy`, `Strict-Transport-Security`).
- **Internationalization** (`lib/i18n/`): All pages live under `/[locale]/`. Server-rendered metadata per locale. Static params generated for `en` and `zh`.
- **OpenNext on Cloudflare**: The site builds as a Cloudflare Worker via `@opennextjs/cloudflare`. Dev-time Cloudflare bindings are initialized in `next.config.ts`.
- **Dynamic data**: GitHub repo stats (stars, forks, issues, PRs, contributors) fetched server-side via `lib/github.ts`. Roadmap feed from `lib/roadmap-feed.ts`. Facts derived at build time (`scripts/derive-facts.mjs` → `lib/facts.generated.ts`).

**Key pages** (under `app/[locale]/`):

| Route | Description |
|---|---|
| `/` (page.tsx) | Landing — hero, stats grid, ticker, seal, release contributors |
| `/install` | Multi-platform install instructions |
| `/docs` | Documentation hub |
| `/roadmap` | Public roadmap with live feed |
| `/faq` | Frequently asked questions |
| `/feed` | Community activity feed |
| `/contribute` | Contributor guide |
| `/admin` | Admin utilities |

**Components:** `Nav`, `Footer`, `Ticker`, `StatGrid`, `Seal` — all server-rendered with RSC.

---

## 2. Session OS Tauri Desktop App (`codew/`)

**Purpose:** A WeChat-style "Session OS" shell for the local CodeWhale runtime. It speaks the documented v1 IDE/thread API — it does **not** reimplement agent logic.

**Tech stack:**

| Layer | Choice |
|---|---|
| Frontend | Vanilla TypeScript SPA (no framework) |
| Bundler | Vite 8 (ES2022 target, port 3000) |
| Shell | Tauri v2 (Rust backend) |
| Desktop platforms | macOS (≥13), Linux, Windows |
| Mobile targets | iOS (≥16), Android (SDK ≥29) |
| Plugins | `store` (persistent storage), `notification` (native notifications), `shell`, `barcode-scanner` |
| Libraries | `qrcode` (QR generation), `jsqr` (QR scanning) |

**Architecture:**

- `index.html` (303 lines): Full app shell — top chrome, session inbox (left), terminal room (center), work rail (right), composer, inspector panel, CodeWhale Link overlay, mobile bottom tabs, command palette, status bar.
- `src/app.ts` (2217 lines): The entire application controller. Owns presentation and control over the local runtime. Key concerns:
  - **Connection management**: Discover local runtimes on common ports; connect with URL + Bearer token; persist settings via `localStorage` (fallback) and Tauri secure store.
  - **Session inbox**: List/filter threads, display status dots (idle/running/completed/failed/blocked), unread approval badges, relative timestamps.
  - **Terminal room**: SSE event stream rendering with tool clustering (consecutive tool events collapse into a single expandable cluster), user prompts, event rows with metadata.
  - **Composer**: Prompt input, plus tray, model selector, auto-approve toggle, interrupt button.
  - **Work rail**: Live display of current turn goal, checklist progress, pending approvals, tool calls, and evidence items.
  - **Inspector panel**: File browser/editor (read, write, FIM fill-in-the-middle), event inspector (full JSON payloads with copy buttons).
  - **CodeWhale Link overlay**: QR code generation, device list, health check, copy/rotate token.
  - **Mobile tabs**: Sessions, Devices (connect/scan), Discover (tasklets), Me (identity/policy).
  - **Command palette**: Typeahead command runner.
  - **Native notifications**: Tauri notification plugin with Web Notification API fallback.
- `src/runtime-client.ts` (443 lines): Typed HTTP/SSE client. Endpoints: `/v1/ide/status`, `/v1/workspace/status`, `/v1/threads/summary`, `/v1/threads` (CRUD), `/v1/threads/{id}/turns` (send/interrupt), `/v1/threads/{id}/events` (SSE with `since_seq` replay).
- `src/link.ts` (226 lines): CodeWhale Link logic — payload codec (`codewhale-link://` scheme), QR generation, transport inference (Tailscale/LAN/local), health probing, device store (localStorage adapter).
- `src-tauri/tauri.conf.json`: Tauri v2 configuration — window size 1440×920, min 980×680, security CSP, bundle config for all platforms, macOS/iOS/Android settings.

**Key features** (from `app.ts`):

| Feature | Implementation |
|---|---|
| Runtime discovery | `RuntimeClient.discover()` probes common ports (7878, 3000, etc.) |
| Connection persistence | `localStorage` + Tauri secure store |
| SSE event streaming | `EventSource` with typed event names (18 event types), `sinceSeq` replay, auto-reconnect |
| Turn lifecycle | `turn.started` → `turn.lifecycle`/`turn.steered` → `turn.completed`/`turn.failed`/`turn.interrupt_requested` |
| Tool clustering | Buffer tool.call/tool.output events; flush as expandable cluster after 140ms |
| Approval workflow | `approval.required` / `approval.decided` / `approval.timeout` events; Work rail approval count |
| File editor | Read/write via `/v1/ide/files/read` and `/v1/ide/files/write`; FIM via `/v1/ide/fim` |
| Mobile scanner | `@tauri-apps/plugin-barcode-scanner` for QR-based CodeWhale Link pairing |

---

## 3. npm Packages (`npm/`)

### 3.1 `codewhale` — CLI Wrapper

**npm package name:** `codewhale`
**Purpose:** Downloads prebuilt `codewhale` and `codewhale-tui` binaries from GitHub Releases and delegates execution to them.

**Bin entries:**
- `codewhale` → `bin/codewhale.js` (runs `codewhale` binary)
- `codew` → `bin/codew.js` (alias)
- `codewhale-tui` → `bin/codewhale-tui.js` (runs `codewhale-tui` binary)

**How it works:**
1. `postinstall` (`scripts/install.js`, 1178 lines): Downloads platform-appropriate binaries from GitHub Releases (or CNB mirror). Verifies checksums against `codewhale-artifacts-sha256.txt` manifest. Glibc preflight on Linux (warns if system glibc is too old). Retries up to 5 times with exponential backoff. Stores binaries in `bin/downloads/`.
2. `scripts/run.js`: Resolves binary path, delegates `process.argv`, falls back to printing version info if binary is missing.
3. `scripts/artifacts.js`: Platform/architecture detection, asset name matrix (linux/macos/windows × x64/arm64/riscv64), release URL construction (GitHub or CNB mirror via `CODEWHALE_USE_CNB_MIRROR` env).

**Release checksum verification:** The npm wrapper downloads `codewhale-artifacts-sha256.txt` from the release, parses it, and validates each downloaded binary's SHA-256 before executing it.

### 3.2 `@codewhale/runtime-sdk` — Runtime Fleet SDK

**npm package name:** `@codewhale/runtime-sdk`
**Purpose:** Typed JavaScript client for CodeWhale Runtime API **fleet** endpoints. Used for orchestrating multiple agent runs in parallel.

**Exports:**
- `CodeWhaleRuntimeClient` class — configurable base URL, token auth, custom fetch implementation.
- `RuntimeApiError` / `RuntimeCapabilityError` — typed errors with status, method, path, body.
- `createRuntimeClient()` — factory function.

**Endpoints:**
- `POST /v1/fleet/runs` — create fleet run
- `GET /v1/fleet/runs` — list runs
- `GET /v1/fleet/runs/{id}` — get run
- `GET /v1/fleet/runs/{id}/workers` — list workers
- `GET /v1/fleet/workers/{id}` — get worker
- `POST /v1/fleet/workers/{id}/interrupt` — interrupt worker
- `POST /v1/fleet/workers/{id}/restart` — restart worker
- `POST /v1/fleet/runs/{id}/stop` — stop run
- `GET /v1/fleet/runs/{id}/events` — SSE event stream (auto-detects JSON array vs. streaming response)

### 3.3 `deepseek-tui` — Deprecated

**npm package name:** `deepseek-tui`
**Status:** **Deprecated.** Private, unpublished compatibility package.
**Purpose:** On `postinstall`, prints a deprecation notice telling users to uninstall `deepseek-tui` and install `codewhale` instead.
**Legacy bin names** (`deepseek`, `deepseek-tui`) still work via symlinks in the Docker image and the codewhale binary's built-in shim dispatch.

---

## 4. REST API Surface (`crates/app-server/src/`)

The app-server is an **Axum** (Rust) HTTP + JSON-RPC stdio server that wraps the CodeWhale runtime.

### 4.1 Transport modes

| Mode | Protocol | Auth |
|---|---|---|
| **HTTP** | REST + SSE over TCP | Bearer token (`Authorization: Bearer cwapp_...`) |
| **Stdio** | JSON-RPC 2.0 (newline-delimited) | None (local process) |

### 4.2 HTTP routes

| Method | Path | Auth | Description |
|---|---|---|---|
| `GET` | `/healthz` | No | Liveness check. Returns `{"status":"ok","protocol":"v2"}` |
| `POST` | `/v1/chat/completions` | No | Provider-neutral OpenAI-compatible pass-through. Resolves model → provider config → forwards upstream. Streaming rejected for now. |
| `POST` | `/thread` | Bearer | Thread operations (create, start, resume, fork, list, read, set_name, goal, archive, stream events) |
| `POST` | `/app` | Bearer | Application-level requests (capabilities, config get/set/unset/list, models list, thread-loaded list, submit user input) |
| `POST` | `/prompt` | Bearer | Prompt execution |
| `POST` | `/tool` | Bearer | Direct tool invocation (with optional `cwd`) |
| `GET` | `/jobs` | Bearer | List background jobs |
| `POST` | `/mcp/startup` | Bearer | MCP server startup |

**CORS:** Default origins include `localhost:1420` (Tauri), `localhost:3000` (dev), `tauri://localhost` (Tauri webview). Configurable via `--cors-origins`.

### 4.3 Stdio JSON-RPC 2.0 methods

The stdio transport supports all the same operations as HTTP, exposed as JSON-RPC methods:

| Method | Description |
|---|---|
| `healthz` | Liveness check |
| `capabilities` | List supported methods and families |
| `thread/create` | Create a new thread |
| `thread/start` | Start a thread |
| `thread/resume` | Resume a paused thread |
| `thread/fork` | Fork a thread |
| `thread/list` | List threads |
| `thread/read` | Read thread details |
| `thread/set_name` | Rename a thread |
| `thread/goal/set` | Set thread goal |
| `thread/goal/get` | Get thread goal |
| `thread/goal/clear` | Clear thread goal |
| `thread/archive` | Archive a thread |
| `thread/unarchive` | Unarchive a thread |
| `thread/message` | Send a message to a thread |
| `app/request` | Application-level request |
| `app/config/get` | Get config value |
| `app/config/set` | Set config value |
| `prompt/request` | Execute a prompt |
| `shutdown` | Graceful shutdown |

### 4.4 SSE Event Streaming

Event streaming happens over HTTP via `ThreadRequest::Stream` with a `since_seq` parameter for replay/resume. The desktop app uses `EventSource` against:

```
GET /v1/threads/{thread_id}/events?since_seq={seq}&token={bearer_token}
```

**Event types** (18 named events):

| Event | Description |
|---|---|
| `thread.started` | A thread has started |
| `thread.forked` | A thread was forked |
| `turn.started` | A new turn began |
| `turn.lifecycle` | Turn lifecycle update |
| `turn.steered` | Turn steering (model direction) |
| `turn.interrupt_requested` | Interrupt was requested |
| `turn.completed` | Turn finished successfully |
| `turn.failed` | Turn ended with error |
| `item.started` | An item (message/tool call) started |
| `item.delta` | Streaming content delta |
| `item.completed` | Item completed |
| `item.failed` | Item failed |
| `item.interrupted` | Item was interrupted |
| `approval.required` | Human approval needed |
| `approval.decided` | Approval decision made |
| `approval.timeout` | Approval timed out |
| `sandbox.denied` | Sandbox execution denied |
| `coherence.state` | Workspace coherence snapshot |

Each event carries `schema_version`, `seq`, `event`, `kind`, `thread_id`, `turn_id`, `item_id`, `timestamp`, and `payload`.

### 4.5 v1 IDE Endpoints

Used by the desktop app for file operations:

| Endpoint | Method | Description |
|---|---|---|
| `/v1/ide/status` | GET | Runtime status: product, workspace, model, provider, FIM support, capabilities |
| `/v1/ide/files/read?path=...` | GET | Read workspace file contents |
| `/v1/ide/files/write` | POST | Write workspace file (`{path, contents}`) |
| `/v1/ide/fim` | POST | Fill-in-the-middle completion (`{prefix, suffix, path}`) |
| `/v1/workspace/status` | GET | Git status: repo, branch, ahead/behind, staged/unstaged/untracked |

---

## 5. CodeWhale Link — Device Pairing

**Purpose:** Local-first device pairing that lets a phone or laptop connect to a CodeWhale runtime over **Tailscale or trusted LAN**. No hosted relay, no public tunnel, no transcript data plane.

**Protocol:**

- **Scheme:** `codewhale-link://runtime?baseUrl=<url>&token=<token>&transport=<transport>[&workspace=<path>][&runtimeVersion=<ver>]`
- **Transports:** `tailscale`, `lan`, `local`
- **QR code:** The desktop app encodes the link payload as a QR code; mobile scans it via `@tauri-apps/plugin-barcode-scanner` or pastes the URL.

**Device model** (`LinkedDevice`):

| Field | Type | Description |
|---|---|---|
| `id` | string | UUID |
| `name` | string | Human-readable device name |
| `kind` | enum | `runtime`, `desktop`, `phone`, `laptop`, `tablet` |
| `baseUrl` | string | Runtime URL |
| `transport` | enum | `tailscale`, `lan`, `local` |
| `linkedAt` | ISO8601 | Pairing timestamp |
| `lastSeen` | ISO8601 | Last health check |

**Health checking:** `probeRuntime()` issues a GET to the runtime's health endpoint. Returns `online`, `token_mismatch`, `offline`, or `unsupported`.

**Storage:** Devices persisted in `localStorage` via a `StorageAdapter` interface (also works with Map-backed adapters in tests).

**Security:** Token-based Bearer auth. The desktop overlay includes a "rotate token" button (requires codewhale runtime support). Mobile includes a "revoke linked device" button for lost-device scenarios.
