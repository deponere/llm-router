# Router-Erweiterungen: Design (5 Features)

Datum: 2026-08-01 · Status: genehmigt (User: „Alle 5, konfigurierbar via Web + CLI")

## Übersicht

Fünf Erweiterungen für den LLM-Router, alle in-process, konfigurierbar via
`config/router.toml` (CLI: `router-admin`) UND Web-UI (neuer „Einstellungen"-Tab):

1. **Router-eigene API-Keys + Budgets** — Auth für die LLM-Endpoints, Quota pro Key
2. **SQLite-Nutzungshistorie** — persistente Transactions + Kosten-Dashboard/Charts
3. **Generalisierte Key-Rotation + Balance-Watchdog** — DeepSeek & Co.
4. **Webhook-/Telegram-Alerts** — Rotation-Fehler, Backend-Down, Balance, Kosten-Schwelle
5. **Benchmark-Panel** — A/B-Vergleich der Top-Kandidaten (TTFT, Latenz, Kosten)

## Architektur

### Config-Schema (`router-config`)

```toml
[server.auth]
enabled = false          # Schutz der LLM-Endpoints (x-api-key / Bearer)
allow_ui = true          # Web-UI (localhost) ist als Admin-Surface freigestellt
# [[server.auth.keys]]   # via CLI/Web generiert — nur SHA-256-Hash in der Config!
# name = "pi", hash = "sha256:…", daily_budget_usd = 2.0, monthly_budget_usd = 30.0

[storage]
db_path = "data/router.sqlite"   # relativ zum Config-Verzeichnis
retention_days = 90

[alerts]
webhook_url = ""                 # optionaler generischer Webhook
telegram_token_env = "TELEGRAM_BOT_TOKEN"
telegram_chat_id = ""
daily_cost_threshold_usd = 0.0   # 0 = aus
[alerts.events]                  # je Event-Typ an/aus (Default: an)
rotation_failed = true
rotation_succeeded = false
backend_down = true
balance_low = true

[backends.<id>]
# bestehende Felder unverändert …
watchdog = { enabled = true, min_balance = 10.0, balance_currency = "USD", check_interval_secs = 3600 }
```

- Secrets-Prinzip: **Key-Hashes statt Plaintext** in `router.toml` (git-getrackt); Plaintext-Key wird genau einmal angezeigt (CLI-Ausgabe / Web-Dialog). Telegram-Token aus Env-Var.

### 1. Auth + Budgets (`auth.rs`)
- Axum-Middleware auf `/v1/chat/completions`, `/v1/messages`, `/v1/explain`, `/v1/benchmark`.
- Header: `x-api-key: <key>` oder `Authorization: Bearer <key>`; SHA-256 gegen Config-Hashes.
- Budget: Tages-/Monatssumme des Keys aus SQLite (`key_name`-Spalte); über Limit → 429.
- Attribution: jede Transaction bekommt `key_name` (Middleware steckt ihn in Request-Extensions).
- Key-Erzeugung: `router-admin auth add <name>` bzw. `POST /v1/admin/keys` → Plaintext einmalig.
- `allow_ui = true`: Requests mit Origin/Referer localhost und sonstiger Pfad /v1/transactions etc. bleiben offen (Admin-Surface), LLM-Endpoints aus Fremd-Clients brauchen Key.

### 2. SQLite-Store (`store.rs`)
- Dependency: `rusqlite` (bundled). DB-Pfad aus `[storage]`, Verzeichnis wird angelegt.
- Tabelle `transactions(unix_ts, api, profile, backend, model_id, model_hint, key_name, tokens_in, tokens_out, cost_usd, duration_ms, error)` + Index auf `unix_ts`.
- `TransactionHistory` (In-Memory) bleibt für Session-KPIs; Store spiegelt parallel (Record-Sites: `openai/mod.rs` + `anthropic/mod.rs`).
- Endpoints: `GET /v1/stats?days=30&key=&group=` → Serie (Kosten/Aufrufe pro Tag), `GET /v1/breakdown?days=7&by=backend|profile|model` → Top-N Summen. UI: Balken-Chart (SVG, dependency-frei) im Usage-Tab + Breakdown-Tabelle.

### 3. Rotation + Watchdog (`rotate.rs` Erweiterung)
- Rotator bleibt (OpenRouter); pro Backend optionaler `watchdog`:
  `GET {base_url}/user/balance` (DeepSeek-kompatibel, `balance_infos[].total_balance` + currency),
  Vergleich gegen `min_balance`, Auslösen eines Alerts (Throttle 1/h). Gleiche 30-s-Tick-Integration wie Rotation.

### 4. Alerts (`alerts.rs`)
- `AlertService` mit pro-Event-Throttle (Default 1/h) — nie blockierend.
- Kanäle: Webhook (POST JSON) + Telegram (`sendMessage`, Token aus Env).
- Event-Quellen: Rotation (failed/succeeded), Watchdog (balance_low), Catalog-Fetch (backend_down), Store-Insert (daily_cost_threshold).

### 5. Benchmark (`benchmark.rs`)
- `POST /v1/benchmark` {messages, models[≤3], max_tokens} → parallele Echt-Calls via `Provider::chat_completion_stream`; misst **TTFT** (Zeit bis erstes Content-Delta) + Gesamtdauer + Tokens + Kosten (Registry-Pricing). UI: Explain-Tab „Benchmark Top 3" mit Warnhinweis (echte Kosten).

### Web/CLI-Konfigurierbarkeit
- Web: neuer Tab „⚙️ Einstellungen" — Auth (an/aus, Keys anlegen/löschen), Storage, Alerts, Watchdog; Speichern → `toml_edit` schreibt Config in-place (Kommentare bleiben) → Auto-Restart.
- CLI: `router-admin auth add|list|rm` (Plaintext-Key einmalig ausgeben), `alerts test`.

## Reihenfolge & Tests
Store → Auth → Alerts → Watchdog → Benchmark → Settings-UI. Jede Phase: `cargo test`, End-Phase: Browser-Verifikation + README/CHANGELOG.
Unit-Tests: Store (in-memory), Auth (Hash/Budget), Alerts (Throttle), Watchdog (Balance-Parse); bestehender E2E bekommt `store`-Feld.
