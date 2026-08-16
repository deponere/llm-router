# Changelog

## Unreleased

### Added
- **Secret-Guard (DLP, Redaktion)** — erkennt Secrets im Prompt auf vier Ebenen (Vault-Radar-inspiriert) und ersetzt sie durch `[REDACTED]`, bevor der Request an ein Backend geht (der Prompt läuft weiter, statt blockiert zu werden): PEM-Private-Keys inkl. SPIFFE-SVID-Key, bekannte Token-Prefixe (`sk-`, `ghp_`, `AKIA`, `AIza`, `xoxb-`, …), strukturierte Formate mit Prüfsumme (Kreditkarten via Luhn, IBAN via mod-97) und ein Shannon-Entropy-Scan für unbekannte hoch-entropische Tokens (base64/base62 ≥ 24 Zeichen H ≥ 4.5, Hex ≥ 40 Zeichen H ≥ 3.5). Geloggt wird nur die Anzahl, nie der Secret-Wert.
- **Key→Profil-Pinning** — `[auth.keys]` kennt jetzt optional `profile = "…"`: der Server erzwingt dieses Profil für jeden Request unter dem Key und ignoriert `x-route-profile`/`route_profile`/`<profile>/auto` vollständig. Ein Key auf ein Profil mit `backend_allowlist = ["omlx"]` kann damit technisch keine Cloud-Backends erreichen. Fehlt das gepinnte Profil in der Config, schlägt der Request fehl (fail-closed, kein stiller Fallback auf `default`). `router-admin auth add … --profile <name>`.
- **Docker-Deployment** — `Dockerfile` (Multi-Stage, rusqlite bundled, non-root, Healthcheck) + `docker-compose.yml` (Port 4123, `./config`-Volume, `env_file: .env`); Key-Rotation im Container über `OPENROUTER_MGMT_KEY` statt macOS-Keychain; oMLX via `host.docker.internal` erreichbar
- **Ollama im Compose-Setup** — zweiter Service (`ollama/ollama`), `config/router.docker.toml` aktiviert das Ollama-Backend (Linux-tauglich, `local`-Profil → ollama), Modell-Pull + Warmup im Entrypoint, `OLLAMA_KEEP_ALIVE=-1`; NVIDIA-GPU-Passthrough auskommentiert enthalten
- **UI: Emojis entfernt** — Navigation, Buttons, Theme-/Sprach-Selector und Logs-Aktionen sind emoji-frei („Chat", „Restart", „Dark/Light/System", „Delete", …)
- **Router API keys + budgets** — `[server.auth]`: SHA-256-hashed `rk_…` keys (plaintext shown exactly once via CLI/Web), `x-api-key`/Bearer middleware on all LLM endpoints, per-key daily/monthly USD budgets enforced from SQLite spend; Web UI stays key-less via same-origin detection. `router-admin auth add|list|rm`, `POST /v1/admin/keys[+/remove]`
- **Persistent usage history (SQLite)** — bundled rusqlite store mirroring every transaction (stream + non-stream, both APIs); `GET /v1/stats` (daily cost series) + `GET /v1/breakdown` (by backend/profile/model/key); retention purge on start; History chart + breakdown tables in the Usage tab
- **Balance watchdog** — per-backend `watchdog = { enabled, min_balance, balance_currency, check_interval_secs }` polls `GET /user/balance` (DeepSeek format) in-process on every request (throttled); alerts on low balance
- **Alerts (webhook + Telegram)** — `[alerts]`: generic webhook POST + Telegram `sendMessage`, per-event throttle (1/h), daily-cost threshold (once/UTC day); events: rotation failed/ok, backend down, balance low, cost threshold; `POST /v1/admin/alerts/test` + `router-admin alerts test`
- **Benchmark panel** — `POST /v1/benchmark`: parallel real calls to up to 3 models, TTFT (first content or reasoning delta), total latency, tokens, cost; „Benchmark top 3" button in the Explain tab (with cost warning)
- **Settings tab (Web) + CLI config** — edit auth/storage/alerts in the web UI (comment-preserving `toml_edit` writes, save → auto-restart), `POST /v1/admin/config`, `GET /v1/admin/config`
- Housekeeping tick on every request: rotation + watchdog + backend health + cost threshold (throttled, never blocking)

### Fixed
- `ServerAuthConfig`/`StorageConfig` defaults when the config section is missing (allow_ui, db_path)
- **UI: Models-/Logs-Tab auf Viewport-Höhe** — nur die Tabelle/Log-Liste scrollt (Sticky-Header bleibt sichtbar), kein Doppel-Scroll der Seite mehr

## 0.1.0-beta.1 — first beta (2026-08-01)

### Added
- Web interface (`GET /` + `/ui`) — LightLLM-style single-file SPA with five tabs: chat playground (SSE streaming, profile/model controls, routing trace), sortable/filterable model catalog, routing explain dry-run (winner, ranking, rejected reasons), usage KPIs + call history, and a live log viewer
- Theme switcher (dark / light / system) in the web interface — persisted in `localStorage` without flash, follows `prefers-color-scheme` in system mode; loguru log colors adapt per theme
- i18n — English default UI with Deutsch / Español / Français switch (persisted, no flash), localized number/date formatting, translated default field values (system prompt, explain sample)
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

### Initial release
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
