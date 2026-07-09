# Changelog

## Unreleased

### Added
- `Provider` trait — arbitrary backends beyond OpenRouter/oMLX: generic `openai_compat` (Ollama, LM Studio, OpenAI, Groq, DeepSeek, xAI, Mistral, Gemini) and native `anthropic` upstreams
- Quality scoring via the Artificial Analysis Intelligence Index: fifth score term (`weights.quality`), `[registry.intelligence]` config with 24 h cache and id→slug aliases, and a `min_intelligence_index` hard filter
- Fallback cascade — on an upstream 4xx/5xx the router streams from the next-best ranked candidate instead of returning the error
- macOS SwiftUI menu-bar admin app (`macos-admin/`) plus `router-admin` — a `dump`/`apply` JSON bridge that rewrites `router.toml` via `toml_edit`, preserving comments and key order

### Changed
- `default` profile prefers oMLX models and denies `:free` variants; unsupported params dropped silently for robustness
- `.cargo/config.toml` pins the real `cc` as compiler/linker (works around a Homebrew node `cc` shim on macOS)
- Dependency + lint cleanup: dropped `tiktoken-rs`, `async-trait` (where unused), `eventsource-stream`, `pin-project-lite`, `tower`, `tokio-stream`; removed the xbar widget (replaced by the admin app)

### Fixed
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
