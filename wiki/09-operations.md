# Operations

CodeWhale's operational infrastructure covers Docker images, install scripts, Nix dev environments, release channels with checksum verification, a private benchmark harness, and a dual CI/CD pipeline (GitHub Actions + CNB mirror).

---

## 1. Dockerfile — Multi-Stage Build

**Location:** `Dockerfile` (in repo root)
**Purpose:** Produce a minimal multi-arch Docker image (linux/amd64, linux/arm64) containing the `codewhale` and `codewhale-tui` binaries.

### Stage 1: Builder (`rust:1.88-slim-bookworm`)

- **Cross-compilation:** Detects `TARGETARCH` (amd64/arm64) and maps to Rust target triples (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`).
- **System deps:** `pkg-config`, `libdbus-1-dev`. On arm64 cross-builds, also installs `gcc-aarch64-linux-gnu` and `libc6-dev-arm64-cross`.
- **Build command:** `cargo build --release --locked --target <triple> -p codewhale-cli -p codewhale-tui`
- **Caching:** Docker BuildKit cache mounts for `target/`, cargo registry, and cargo git checkouts.
- **Output:** Copies `codewhale` and `codewhale-tui` binaries to `/out/`.

### Stage 2: Runtime (`debian:bookworm-slim`)

- **System deps:** `ca-certificates`, `libdbus-1-3` (runtime only, no dev headers).
- **User:** Non-root `codewhale` user (UID 1000, GID 1000), home at `/home/codewhale`.
- **Data dirs:** `.codewhale` and `.deepseek` directories created with `0700` permissions.
- **Legacy symlinks:** `deepseek → codewhale`, `deepseek-tui → codewhale-tui` for backward compatibility with the `deepseek-tui` npm era.
- **Entrypoint:** `codewhale` (dispatcher).
- **Volumes:** `/home/codewhale/.codewhale` — mount for persistent state.

### Usage

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t codewhale:latest .
docker run --rm -it -e DEEPSEEK_API_KEY -v codewhale-home:/home/codewhale/.codewhale codewhale
```

API keys are always passed at runtime (`-e` or `--env-file`), never baked into the image.

---

## 2. Install Scripts

### 2.1 `scripts/release/install.sh` — Unix Installer

**What it does:**
1. Copies `codewhale` and `codewhale-tui` from the release archive to `$PREFIX/bin` (default: `~/.local/bin`).
2. **Glibc preflight check:** Extracts the highest `GLIBC_X.Y` symbol requirement from each binary, detects the host glibc version (via `getconf GNU_LIBC_VERSION` or `ldd --version`), and warns if the binary was built against a newer glibc than the system provides. Can be bypassed with `CODEWHALE_SKIP_GLIBC_CHECK=1`.
3. Prints PATH setup instructions for zsh, bash, fish.

### 2.2 `scripts/release/install.bat` — Windows Installer

**What it does:**
1. Copies `codewhale.exe` and `codewhale-tui.exe` to `%USERPROFILE%\bin`.
2. Prints PATH setup instructions (GUI or PowerShell).

### 2.3 `npm/codewhale/scripts/install.js` — npm Postinstall (1178 lines)

This is the most sophisticated installer. On `npm install -g codewhale`, it:
1. Detects platform/architecture via `artifacts.js`.
2. Downloads the binary and its companion (`codewhale-tui`) from GitHub Releases (or CNB mirror if `CODEWHALE_USE_CNB_MIRROR` is set).
3. **Checksum verification:** Downloads `codewhale-artifacts-sha256.txt` manifest, parses it, and validates each downloaded binary's SHA-256.
4. **Glibc preflight** on Linux (same check as `install.sh`).
5. **Retry logic:** Up to 5 attempts with exponential backoff (1s → 2s → 4s → 8s → 16s), 5-minute timeout per attempt, 30s stall detection. During `postinstall` (optional mode): 1 attempt, 15s timeout, 5s stall — fails fast so the user can recover on first manual run.
6. Stores binaries in `bin/downloads/` (within the npm package).

### 2.4 `scripts/installer/codewhale.nsi` — NSIS Windows Installer

Used in the release workflow to build `CodeWhaleSetup.exe` via NSIS (Nullsoft Scriptable Install System). Combines both binaries into a single Windows installer.

---

## 3. Nix / Flake Development Environment

**Files:** `flake.nix`, `flake.lock`, `nix/package.nix`

### Flake (`flake.nix`)

- **Inputs:** `nixpkgs` (nixos-unstable), `fenix` (nix-community/fenix — Rust toolchain).
- **Supported systems:** `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`.
- **Overlay:** `rustToolchain` — fenix stable channel with `rustc`, `cargo`, `clippy`, `rustfmt`, `rust-src`.
- **Packages:**
  - `default` / `codewhale`: builds `codewhale-cli` and `codewhale-tui` from source via `buildRustPackage`.
  - `deepseek-tui`: compatibility alias → `codewhale`.
- **Dev shell:** `rustToolchain`, `rust-analyzer`, `lldb`, `pkg-config`, `openssl`, `python3`, `nixfmt`. On Linux: `dbus`. Sets `RUST_SRC_PATH` for rust-analyzer and `LD_LIBRARY_PATH` for openssl/dbus.
- **Formatter:** `nixfmt`.

### Package (`nix/package.nix`)

- Uses `rustPlatform.buildRustPackage`.
- `cargoBuildFlags`: `-p codewhale-cli -p codewhale-tui`.
- `cargoTestFlags`: same packages, `--lib --bins`.
- Build inputs: `openssl`, `dbus.dev`, `dbus.lib` (Linux), `stdenv.cc.cc.lib` (Linux).
- Check inputs: `python3`, `gitMinimal`, `cacert`.
- Uses `autoPatchelfHook` on Linux.
- Version set from git rev: `git-${rev}`.

### Usage

```bash
nix develop          # enter dev shell
nix build            # build codewhale
nix run . -- --help  # run codewhale
```

---

## 4. Release Channels

### 4.1 Stable vs Beta

Defined in `crates/release/src/lib.rs` (`ReleaseChannel` enum):

| Channel | Source | Discovery |
|---|---|---|
| **Stable** | Latest GitHub Release | `GET /repos/Hmbown/CodeWhale/releases/latest` |
| **Beta** | Pre-release versions | `GET /repos/Hmbown/CodeWhale/releases?per_page=100` (filters pre-releases) |

**Control flow:**
- CLI flag `--beta` selects the Beta channel.
- Environment variable `DEEPSEEK_TUI_VERSION` (or `DEEPSEEK_VERSION`) pins the update target version.
- `CODEWHALE_RELEASE_BASE_URL` (or legacy `DEEPSEEK_TUI_RELEASE_BASE_URL` / `DEEPSEEK_RELEASE_BASE_URL`) overrides the download base URL.
- `CODEWHALE_USE_CNB_MIRROR` switches to the CNB (China-friendly) mirror for release downloads.

### 4.2 Release Query Resolution

```
resolve_release_query(channel):
  1. If CODEWHALE_RELEASE_BASE_URL is set → Mirror query (custom URL + pinned version)
  2. If channel == Stable → GitHubLatest
  3. If channel == Beta  → GitHubReleaseList
```

### 4.3 Release Crate (`crates/release/`)

**Package:** `codewhale-release`
**Purpose:** Shared release discovery and version comparison helpers used by the CLI's updater.

**Key constants:**
- `CHECKSUM_MANIFEST_ASSET`: `codewhale-artifacts-sha256.txt`
- `LATEST_RELEASE_URL`: GitHub API latest-release endpoint
- `RELEASES_URL`: GitHub API release-list endpoint
- `CNB_REPO_URL`: CNB mirror URL
- `UPDATE_USER_AGENT`: `codewhale-updater`
- `RELEASE_METADATA_TIMEOUT`: 5 seconds

**Dependencies:** `reqwest` (blocking), `semver`, `serde`, `serde_json`.

---

## 5. Checksum Verification

### 5.1 How it works

Every release includes a **checksum manifest** (`codewhale-artifacts-sha256.txt`) in standard `sha256sum` format:

```
<sha256>  codewhale-linux-x64
<sha256>  codewhale-tui-linux-x64
<sha256>  codewhale-macos-arm64
...
```

### 5.2 Manifest Generation (in CI)

The release workflow's `bundle` job:
1. Groups each binary + install script into platform bundles (`.tar.gz` or `.zip`).
2. Computes `sha256sum` for each archive.
3. Appends to the manifest (`codewhale-artifacts-sha256.txt`).

The release workflow's `release` job regenerates a comprehensive manifest:
1. Lists all artifact files.
2. Runs `sha256sum` on each file.
3. Writes the canonical `codewhale-artifacts-sha256.txt`.

Both the per-binary checksums (from npm install) and the per-archive checksums (from release) are published as release assets.

### 5.3 Verification at Install Time

The npm wrapper (`scripts/install.js`) downloads the manifest first:
```javascript
// artifacts.js
function checksumManifestUrl(version, repo) {
  return releaseAssetUrl("codewhale-artifacts-sha256.txt", version, repo);
}
```

Then validates each downloaded binary against the manifest before marking it executable.

### 5.4 Homebrew Tap

The release workflow updates a Homebrew tap (`Hmbown/homebrew-deepseek-tui`). The tap formula references the checksum manifest for bottle verification.

---

## 6. Benchmark Infrastructure (`codewhale-bench/`)

**Location:** Private repo at `codewhale-bench/`
**Purpose:** Reproducible **raw-vs-CodeWhale** harness comparison. Measures how much the CodeWhale agent harness adds (or subtracts) relative to calling the same model directly through its native API.

### 6.1 Benchmarks Covered

| Benchmark | What it measures |
|---|---|
| **Tau-bench / τ³** | Agentic task completion in simulated retail/airline environments. Three conditions: `raw` (model API only), `codewhale` (full harness), `codewhale-bare` (ablation — harness without tools). |
| **Prime Eval** | Static capability benchmarks: GPQA-Diamond, AIME25, MMLU-Pro. Raw provider endpoints vs. CodeWhale OpenAI-compatible proxy. |
| **Terminal-Bench 2.1** | Real-world shell tasks. Raw Arcee/DeepSeek runners vs. CodeWhale runners using bundled Linux binary inside Harbor containers. |
| **MMLU-Pro Full-Harness Smoke** | Small standalone smoke comparing raw Arcee vs. the full CodeWhale runtime (thread/turn API, not just `/v1/chat/completions`). |

### 6.2 Runner Architecture

- **Two-layer separation:** Benchmark-specific runners preserve upstream semantics (defaults, scoring, task selection). The CodeWhale meta-harness standardizes launch config, traces, records, summaries, provider routing, and raw-vs-CodeWhale comparisons.
- **Vendored CodeWhale:** The benchmark repo vendors CodeWhale source under `vendor/codewhale/` with a `.bench-source-ref` marker tracking exact commit SHA. A patch set (5 patches in `patches/codewhale/`) applies the ablation surface, turn temperature control, system-prompt dump, and Arcee streaming usage fixes.
- **Harbor integration:** Terminal-Bench tasks run inside Docker containers orchestrated by Harbor. A `scripts/prepare-codewhale-terminal-bench.sh` builds the Linux CodeWhale bundle that Harbor uploads into task containers.

### 6.3 Ablation Surface

The critical patch (`runtime-api-delegated-tools`) introduces:
- `HarnessProfile` enum: `Full` (all tools) vs. `Bare` (no tools — pure model).
- `CODEWHALE_HARNESS_PROFILE` env var selects profile.
- `CODEWHALE_RUNTIME_DELEGATED_ONLY` restricts to delegated tools only.
- This enables apples-to-apples comparison: same model, same task, with and without the CodeWhale tool harness.

### 6.4 Setup & Running

Requires: Docker, `uv`, provider API keys (Arcee, DeepSeek, OpenAI, OpenRouter). See `codewhale-bench/README.md` (345 lines) for full runbook.

---

## 7. CI/CD Pipeline

CodeWhale runs a **dual CI/CD** pipeline: GitHub Actions for public-facing CI and release automation, plus a CNB (China-friendly) mirror for Linux-heavy gates and Chinese-region release artifacts.

### 7.1 GitHub Actions (`.github/workflows/`)

#### `ci.yml` — Main CI (push/PR to master/main, weekly schedule)

| Job | What it does |
|---|---|
| **versions** | Version drift check (`check-versions.sh`), OHOS dependency graph check |
| **lint** | `cargo fmt --check`, `cargo clippy --workspace --all-features`, provider registry drift check, co-author trailer validation |
| **test** | `cargo test --workspace --all-features` on macOS + Windows (Linux tests run on CNB) |
| **npm-wrapper-smoke** | Build binaries, run `npm-wrapper-smoke.js` (validates npm wrapper delegates to real binaries). On PR: ubuntu only. On push: ubuntu + macOS + Windows. |
| **mobile-smoke** | `scripts/mobile-smoke.sh` — mobile runtime smoke tests (ubuntu) |
| **docs** | `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS: -Dwarnings` (weekly schedule only) |

#### `release.yml` — Release Pipeline (tag push `v*` or `workflow_dispatch`)

| Stage | What it does |
|---|---|
| **parity** | Full workspace gates: fmt, check, clippy, test, protocol parity test, state parity test, lockfile drift |
| **resolve** | Resolves release tag, source ref, and SHA (handles both tag push and manual dispatch) |
| **build** | Builds 14 platform binaries: `codewhale` (linux-x64/arm64/riscv64, macos-x64/arm64, windows-x64) + `codewhale-tui` (same targets + linux-x64 musl variant) |
| **bundle** | Groups binaries + install scripts into `.tar.gz`/`.zip` archives, generates per-archive checksum manifest |
| **windows-installer** | Builds `CodeWhaleSetup.exe` via NSIS |
| **docker** | Multi-arch Docker build (linux/amd64 + linux/arm64), pushes to `ghcr.io` with semantic version tags |
| **release** | Creates GitHub Release with all artifacts (binaries, bundles, checksums, Windows launcher `.bat`), generates release body from `CHANGELOG.md` |
| **homebrew** | Updates Homebrew tap formula with new version and checksums |

#### `nightly.yml` — Nightly Builds (push to main or manual dispatch)

Builds all 12 platform binaries on every push to `main`. Caches with `Swatinem/rust-cache`. RISC-V cross-compilation uses `gcc-riscv64-linux-gnu`.

#### `web.yml` — Web Frontend

| Job | What it does |
|---|---|
| **lint** | ESLint + TypeScript type check (`tsc --noEmit`) |
| **deploy** | Build OpenNext bundle, deploy to Cloudflare (manual dispatch on `main` only) |

#### Other workflows

| Workflow | Purpose |
|---|---|
| `auto-tag.yml` | Automated tagging |
| `stale.yml` | Stale issue/PR management |
| `triage.yml` | Issue triage automation |
| `issue-gate.yml` | Issue quality gates |
| `pr-gate.yml` | PR quality gates |
| `auto-close-harvested.yml` | Auto-close issues harvested for releases |
| `sync-cnb.yml` | Syncs GitHub → CNB mirror |
| `spam-lockdown.yml` | Spam prevention |
| `approve-contributor.yml` | Contributor approval workflow |

### 7.2 CNB Pipeline (`.cnb.yml`)

CNB is a one-way mirror from GitHub. The CNB pipeline handles Linux-heavy gates that are redundant on GitHub Actions but necessary for Chinese-region users.

**Push to `main` / `fix/*` / `rebrand/*`:**
- **feishu bridge tests:** Install + test the Feishu (Lark) integration bridge.
- **linux rust gates:** Full workspace gates (fmt, clippy, test, parity tests, npm wrapper smoke) in a `rust:1.88-bookworm` Docker container with 16 CPUs.

**Push to `work/v*` (release branches):**
- All of the above, plus:
  - **Crate publish dry-run**
  - **Release binary smoke** (build release binaries, run npm wrapper smoke, verify `--version`)

**Tag push:**
- Builds **static** `x86_64-unknown-linux-musl` binaries.
- Strips debug symbols.
- Generates SHA-256 checksums.
- Creates a CNB release with CHANGELOG excerpt and asset uploads.

### 7.3 Version Drift Checks (`scripts/release/check-versions.sh`)

Runs on every CI push. Checks:
1. No crate uses literal `version = "x.y.z"` — must use `version.workspace = true`.
2. npm wrapper version matches workspace `Cargo.toml` version.
3. Internal `codewhale-*` dependency pins match workspace version.
4. TUI crate's packaged changelog matches root `CHANGELOG.md`.
5. Current release has a dated Keep a Changelog entry.
6. `SECURITY.md` keeps the dedicated security contact.
7. `codewhale-app-server` stays library-only.
8. `Cargo.lock` is in sync (`cargo metadata --locked`).

### 7.4 Release Preparation (`scripts/release/prepare-release.sh`)

Bumps all version-bearing files in one pass:
1. Workspace `Cargo.toml` version
2. All `crates/*/Cargo.toml` internal dependency pins
3. `npm/codewhale/package.json` version + `codewhaleBinaryVersion`
4. All README translations (install-tag examples)
5. `crates/tui/CHANGELOG.md` (via `sync-changelog.sh`)
6. `web/lib/facts.generated.ts` (via `derive-facts.mjs`)
7. Regenerates `Cargo.lock`

Does NOT write the CHANGELOG entry — that must be added manually first.
