# Changelog

## Unreleased

### Added
- Web interface (`GET /` + `/ui`) — LightLLM-style single-file SPA with five tabs: chat playground (SSE streaming, profile/model controls, routing trace), sortable/filterable model catalog, routing explain dry-run (winner, ranking, rejected reasons), usage KPIs + call history, and a live log viewer
- Live log viewer — in-memory ring buffer (last 500 entries) fed by a dedicated tracing layer, exposed via `GET /v1/logs` / `POST /v1/logs/clear`, rendered loguru-style (millisecond timestamps, padded level colors, `target:line`) with level filter, search, pause, clear and copy
- Automatic OpenRouter key rotation — on every request (throttled to a real check every 30 s) the router rotates the inference key via the Management API (`/api/v1/keys`) when process uptime or key age exceeds `OPENROUTER_ROTATE_DAYS`; the management key lives in the macOS Keychain (never plaintext), `OPENROUTER_LIMIT` / `OPENROUTER_LIMIT_RESET` configure the per-key budget
- `POST /v1/admin/restart` — restart the router process in a new session (survives terminal close); `main` retries the bind so the new process takes over the port cleanly
- `scripts/store-key.sh` (Keychain storage via `read -s`, no shell history) and `scripts/rotate-openrouter-key.sh` (manual rotation tool: `--status` / `--check` / `--force` / `--dry-run`)
- `Provider` trait — arbitrary backends beyond OpenRouter/oMLX: generic `openai_compat` (Ollama, LM Studio, OpenAI, Groq, DeepSeek, xAI, Mistral, Gemini) and native `anthropic` upstreams
- Quality scoring via the Artificial Analysis Intelligence Index: fifth score term (`weights.quality`), `[registry.intelligence]` config with 24 h cache and id→slug aliases, and a `min_intelligence_index` hard filter
- Fallback cascade — on an upstream 4xx/5xx the router streams from the next-best ranked candidate instead of returning the error
- macOS SwiftUI menu-bar admin app (`macos-admin/`) plus `router-admin` — a `dump`/`apply` JSON bridge that rewrites `router.toml` via `toml_edit`, preserving comments and key order

### Changed
- Default bind port `127.0.0.1:4000` → `127.0.0.1:4123` (config, README, admin-app fallback, test fixtures)
- DeepSeek backend enabled by default; credential resolved from the `DEEPSEEK_API_KEY` env var instead of an inline value
- oMLX base URL moved to `127.0.0.1:8008`; explicit `local = false` on cloud backends
- README: repo links updated to `github.com/deponere/llm-router` after the rename
- `default` profile prefers oMLX models and denies `:free` variants; unsupported params dropped silently for robustness
- `.cargo/config.toml` pins the real `cc` as compiler/linker (works around a Homebrew node `cc` shim on macOS)
- Dependency + lint cleanup: dropped `tiktoken-rs`, `async-trait` (where unused), `eventsource-stream`, `pin-project-lite`, `tower`, `tokio-stream`; removed the xbar widget (replaced by the admin app)

### Fixed
- Hardcoded DeepSeek API key in `config/router.toml` (pasted into the `env` field, which expects a variable name — the lookup always failed) — key moved to `.env`
- Restart-spawned processes inherited the parent's stdout pipe; once the pipe buffer filled, log writes blocked and froze the tokio runtime — stdio is now redirected to `/dev/null` unless attached to a TTY
- Spec-compliant Anthropic stream closing sequence
- Honour `stream: false` on both APIs; aggregate `tool_calls` and usage for non-stream responses
- Case-insensitive oMLX registry-override matching; AA client uses the `slug` field and strips `:free`

## 0.1.0

- Initial public release
- OpenAI-compatible endpoints: `GET /v1/models`, `POST /v1/chat/completions` (stream + non-stream)
- Anthropic-compatible endpoint: `POST /v1/messages` with transparent OpenAI↔Anthropic translation (including `thinking`, tool use, image content blocks)
- Deterministic expert system: profile resolve → hard filters → weighted scoring → provider flags (no LLM-in-the-loop, no random sampling)
- Built-in profiles: `default`, `cheap`, `fast`, `private`, `smart`, `local` — fully configurable via `config/router.toml`
- Backends: OpenRouter (cloud meta-provider, ~300 models) and oMLX (local Apple Silicon MLX inference)
- Hard filters: context window, modalities, capabilities (`tools` / `structured_outputs` / `reasoning`), privacy class (`Local` / `Zdr` / `Standard`), output-price cap, backend allowlist
- Scoring inputs: cost, latency (p95), context headroom, preference — weights sum to 1 per profile, tiebreak on `(backend_priority, model_id)`
- OpenRouter `provider.*` block injection: `require_parameters`, `sort`, `zdr`, `allow_fallbacks`, `data_collection`, `quantizations` — derived from the active profile
- Registry overrides in TOML for modalities and capabilities the backend `/models` endpoint doesn't report
- Per-request routing via `x-route-profile` header, `route_profile` body field, or `privacy_tag` body field
- `LatencyTracker` with per-model 200-sample ring buffer and live p50/p95 feeding `latency_score`
- Model registry cached with 5-minute Moka TTL; SSE streamed end-to-end from egress to caller
- Drop-in `.env.example`, workspace split into `router-config` / `router-core` / `router-providers` / `router-api`
- Test coverage: unit tests for expert-system phases, wiremock-backed E2E tests against a simulated OpenRouter
