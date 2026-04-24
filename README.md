# router

A local proxy that speaks the OpenAI and Anthropic APIs and routes every request to the best available model via a deterministic expert system — no random sampling, no LLM-in-the-loop, no surprises.

**Backends**: [OpenRouter](https://openrouter.ai/) (cloud, ~300 models) and [oMLX](https://omlx.ai/) (local Apple Silicon inference).

## Get started

```bash
git clone https://github.com/deponere/router.git
cd router
cp .env.example .env          # set OPENROUTER_API_KEY
cargo run -p router-api --release
```

Then point any OpenAI or Anthropic client at `http://127.0.0.1:4000`. See [Quick start](#quick-start) for a full request example.

## Learn more

- [How it works](#how-it-works) — the expert-system pipeline in four phases
- [Profiles](#profiles) — the six built-in routing profiles and their weights
- [Configuration](#configuration) — `config/router.toml` reference
- [CHANGELOG](./CHANGELOG.md) — release notes

## Reporting bugs

Open an issue on [github.com/deponere/router/issues](https://github.com/deponere/router/issues) with the request payload, the `x-route-profile` you used, and the log output from `RUST_LOG=router_core=debug`.

---

## How it works

```
Client (OpenAI or Anthropic format)
  │
  ▼
Ingress (Axum)              /v1/chat/completions  /v1/messages  /v1/models
  │
  ▼
Request Normalizer          unifies both API formats into NormRequest
  + Feature Detector        detects required caps: vision · tools · json · reasoning
  │
  ▼
Expert System               purely deterministic — fixed rule order
  Phase 1  profile resolve  x-route-profile header or route_profile body field
  Phase 2  hard filter      context window · modalities · caps · privacy · price · backend
  Phase 3  scoring          cost + latency + context headroom + preference (configurable weights)
  Phase 4  provider flags   injects provider.* block for OpenRouter requests
  │
  ▼
Egress client               streams SSE directly back to the caller
  ├── OpenRouter             cloud meta-provider with full provider routing control
  └── oMLX                  local MLX server on Apple Silicon (OpenAI-native SSE)
```

The expert system never makes an LLM call to decide routing. Given an identical request and an identical model registry snapshot, it always picks the same model.

---

## Quick start

```bash
cp .env.example .env
# edit .env — set OPENROUTER_API_KEY (and optionally OMLX_HOST)

cargo run -p router-api

# in another terminal:
curl -s http://127.0.0.1:4000/v1/models | jq '.data | length'

curl -sN http://127.0.0.1:4000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'x-route-profile: cheap' \
  -d '{"model":"auto","stream":true,"messages":[{"role":"user","content":"hi"}]}'
```

---

## Profiles

Select a profile via the `x-route-profile` header or the `route_profile` body field.

| Profile | Intent | Key constraints |
|---------|--------|-----------------|
| `default` | balanced | cost 35 % · latency 25 % · context 15 % · preference 25 % |
| `cheap` | minimize cost | max $2/Mtok out · sort by price |
| `fast` | minimize latency | max 1 500 ms p95 · sort by latency |
| `private` | no data leaving controlled providers | ZDR or local only · `allow_fallbacks=false` |
| `smart` | best quality | Claude Opus / GPT-5 / Gemini 2.5 Pro · fp16/bf16/fp8 only |
| `local` | oMLX only | `backend_allowlist=["OMlx"]` · zero cloud egress |

Weights, caps, and hard limits for each profile live in `config/router.toml`.

---

## Expert system — phases in order

### Phase 1 — profile resolution

`x-route-profile` header or `route_profile` body field → loads profile from `router.toml`. Falls back to `[profiles.default]`.

### Phase 2 — hard filters (first failure eliminates the candidate)

1. **Context window** — `prompt_tokens_est + max_tokens_reserve ≤ model.context_length`
2. **Modalities** — request modalities ⊆ model input modalities (e.g. image → vision model required)
3. **Capabilities** — tools / structured_outputs / reasoning must be in `model.supported_parameters`
4. **Privacy class** — profile may require `Local` or `Zdr`; `Standard` models are excluded
5. **Output price cap** — `profile.max_price_out_per_mtok` hard ceiling
6. **Backend allowlist** — profile may restrict to `OpenRouter` or `OMlx`

### Phase 3 — scoring

```
score = w_cost    · cost_score(model, profile)
      + w_latency · latency_score(model.p95_ms)
      + w_context · context_headroom_score(model, prompt_tokens)
      + w_pref    · preference_score(model, profile.preferences)
```

All sub-scores normalised to [0, 1]. Weights sum to 1 and come from the active profile. Tiebreak: `(backend_priority, model_id)` lexicographically — documented in `router-core/src/score.rs` and fixed by a unit test.

### Phase 4 — OpenRouter provider flags

For the winning OpenRouter candidate the `provider.*` block is injected:

- `require_parameters = true` — no silent parameter drops
- `sort` from profile (`"price"` / `"latency"` / `"throughput"`)
- `zdr = true` and `allow_fallbacks = false` in `private` profile
- `data_collection = "deny"` for ZDR/LocalOnly privacy tags
- `quantizations` restriction in `smart` profile (`["fp16","bf16","fp8"]`)

oMLX requests get the `provider` key stripped before forwarding.

---

## API endpoints

The proxy is a drop-in replacement for both OpenAI and Anthropic clients.

### OpenAI-compatible

```
GET  /v1/models                       merged model list (OpenRouter + oMLX)
POST /v1/chat/completions             stream or non-stream, auto routing
```

Extra fields accepted in the request body:

| Field | Type | Description |
|-------|------|-------------|
| `model` | `"auto"` or model id | `"auto"` triggers routing; an explicit id pins the model |
| `route_profile` | string | same as `x-route-profile` header |
| `privacy_tag` | `"normal"` \| `"zdr"` \| `"local_only"` | per-request privacy override |

### Anthropic-compatible

```
POST /v1/messages                     Anthropic Messages API (stream or non-stream)
```

Accepts the full Anthropic request format including `thinking`, tool use blocks, and image content. The proxy translates to OpenAI internally and translates the response back before returning.

---

## Configuration

`config/router.toml` is the single source of truth. Set `ROUTER_CONFIG` env var to override the path.

### Backends

```toml
[backends.openrouter]
enabled = true
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"

[backends.omlx]
enabled = true
base_url_env = "OMLX_HOST"
base_url_default = "http://127.0.0.1:8000"
```

### Registry overrides

Used to patch modalities or capabilities that the backend's `/models` endpoint doesn't report:

```toml
[[registry.overrides]]
backend = "OMlx"
id_prefix = "qwen3"
input_modalities = ["text"]
caps = ["tools"]
```

### Privacy classification

Maps OpenRouter provider slugs to privacy classes:

```toml
[registry.privacy]
local = []
zdr  = ["anthropic", "openai", "google"]
```

Unlisted slugs → `Standard`.

### Custom profile

```toml
[profiles.my_profile]
weights = { cost = 0.6, latency = 0.2, context = 0.1, preference = 0.1 }
max_price_out_per_mtok = 5.0
provider_sort = "price"
provider_require_parameters = true
```

---

## Environment variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENROUTER_API_KEY` | if OpenRouter enabled | — | OpenRouter API key |
| `OMLX_HOST` | no | `http://127.0.0.1:8000` | oMLX server base URL |
| `OMLX_API_KEY` | no | — | oMLX auth token (if configured) |
| `ROUTER_CONFIG` | no | `config/router.toml` | path to config file |
| `RUST_LOG` | no | `info` | tracing filter (e.g. `router_core=debug`) |

---

## Workspace layout

```
crates/
├── router-config/      TOML loader — Config, Profile, Weights
├── router-core/        expert system — norm · registry · rules · score · decision
├── router-providers/   egress clients — OpenRouter · oMLX · LatencyTracker
└── router-api/         Axum server — OpenAI + Anthropic handlers · routes
config/
└── router.toml         profiles, backends, registry overrides
.env.example            env var template
```

---

## Development

```bash
# run all tests (unit + integration with wiremock)
cargo test

# run only the core expert-system tests (no network)
cargo test -p router-core

# run E2E tests with mocked OpenRouter
cargo test -p router-api --test e2e_openrouter

# release build
cargo build --release

# live log of routing decisions
RUST_LOG=info,router_core=debug cargo run -p router-api
```

The model registry is cached for 5 minutes (moka TTL). A restart forces a fresh fetch.

---

## Latency tracking

Every successful completion records the time-to-first-token. The `LatencyTracker` keeps a per-model ring buffer (last 200 samples) and exposes p50 and p95 percentiles. These feed directly into the `latency_score` in Phase 3. Before enough samples accumulate the score falls back to zero (unknown latency → neutral, not penalised).

---

## Limitations / non-goals (v0.1)

- No fallback cascade — a 503 from the chosen model is returned to the caller as-is.
- No persistent metrics — latency data resets on restart.
- No hot-reload — config changes require a restart.
- Claude Max excluded — Anthropic's April 2026 ToS prohibits third-party proxying of Max subscriptions.
