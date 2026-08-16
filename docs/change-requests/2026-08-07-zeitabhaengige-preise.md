# Change Request: Zeitabhängige Preise & Nutzungszeiten pro LLM-Anbieter

> **Umsetzungsstand (2026-08-07):** R1–R3 umgesetzt (Backend-`blocked_windows` mit
> `days = every|weekdays|weekend`, UTC, Wrap-around; Hard-Filter `TimeBlocked` in
> `rules.rs`; Settings-UI „Backends"-Editor). **R4 entfällt wie geplant:** OpenRouter
> liefert in `/models` aktuell KEINE zeitabhängigen Fenster — das `pricing.overrides`-
> Array ist token-schwellenbasiert (`min_prompt_tokens`), nicht zeitbasiert. Es gibt
> also nichts Zeitbasiertes zu ziehen; die manuellen Backend-Fenster sind der Mechanismus.

- **Status:** Entwurf zur Review
- **Datum:** 2026-08-07
- **Autor:** Markus (via Hermes)
- **Betroffen:** llm-router (alle Crates + Web-UI)
- **Aufwandsschätzung:** ~1,5 Tage (Config-Schema + Filter + OpenRouter-Pull + UI)

## Kontext / Problem

LLM-Anbieter führen zunehmend zeitabhängige Preise ein (z. B. DeepSeek mit
Off-Peak-Rabatten bzw. in der Vergangenheit höheren Preisen in bestimmten
Zeitfenstern). Der Router kennt heute nur statische Preise pro Modell und
kann daher zu teuren Zeiten Modelle/Anbieter auswählen, die der Nutzer zu
diesen Zeiten nicht verwenden will. Es fehlt:

1. eine Möglichkeit, pro LLM-Anbieter (Backend) Nutzungszeiten zu definieren
   („darf von X bis Y **nicht** genutzt werden“),
2. die Einstellbarkeit im Web-Interface,
3. die automatische Übernahme und Anzeige von Zeitfenstern, wenn der Anbieter
   sie liefert (OpenRouter liefert pro Modell ein `pricing.overrides`-Array,
   das Peak-/Off-Peak-Fenster über den gesamten 24h-Tag abdeckt), plus
   Auswahl pro Modell, ob das Fenster gesperrt werden soll.

## Ziele

- Kostenkontrolle: keine Requests an teure Fenster, wenn der Nutzer sie sperrt.
- Sichtbarkeit: Preisfenster werden im UI angezeigt (Models-Tab, Backend-Editor).
- Automatik: OpenRouter-Fenster werden beim Catalog-Fetch mitgezogen und
  angezeigt; der Nutzer entscheidet per Toggle, ob ein Fenster gesperrt wird.
- Nachvollziehbarkeit: gesperrte Kandidaten erscheinen im Routing-Trace und im
  Log mit Grund `time_blocked`.

## Anforderungen

- **R1 (Config):** Pro Backend konfigurierbare Sperrfenster:
  ```toml
  [backends.deepseek]
  enabled = true
  # UTC-Zeitfenster (HH:MM), in denen das Backend NICHT genutzt werden darf.
  # Wrap-around erlaubt (start > end = über Mitternacht).
  blocked_windows = [{ start = "07:00", end = "09:00" }]
  ```
  Default: keine Sperre. Zeitbasis UTC (keine TZ-Konfiguration im ersten
  Schritt — offene Frage, s. u.).

- **R2 (Routing):** Sperrfenster sind ein Hard-Filter in `router-core`
  (`rules.rs`), nicht nur ein Score-Abzug: In einem gesperrten Fenster fällt
  das Backend/Modell komplett aus der Kandidatenliste und der Router weicht
  auf erlaubte Modelle aus. Neuer `FilterReason::TimeBlocked`, sichtbar im
  `DecisionTrace` (rejected) und in der Log-Zeile `router decision`.

- **R3 (Web-UI):**
  - Settings-Tab: neuer Abschnitt „Backends“ — Liste aller konfigurierten
    Backends mit enabled/disabled und editierbaren Sperrfenstern
    (kommagetrennt `07:00-09:00, 22:00-06:00`), gespeichert über den
    bestehenden `POST /v1/admin/config`-Mechanismus (dotted keys,
    TOML-Array-Support existiert bereits).
  - Models-Tab: neue Spalte „Preis-Fenster“ — zeigt für OpenRouter-Modelle die
    vom Provider gelieferten Fenster (z. B. `peak 00:30–16:30`, `off-peak
    16:30–00:30 (−50 %)`) und je Fenster einen Toggle „sperren“.
  - Der Toggle persistiert als Modell-Sperre in der Config (neue Sektion
    `[registry.time_blocks]`, s. Design), nicht nur in-memory.

- **R4 (OpenRouter-Pull):** Der OpenRouter-Provider parst beim
  Catalog-Fetch das `pricing.overrides`-Array aus `/models` und legt die
  Fenster (Zeiten + Preise) auf dem `ModelCandidate` ab
  (`pricing_windows: Vec<PricingWindow>`). Die exakten Feldnamen
  (`start_time`/`end_time`/Preisfelder) sind im Implementierungsschritt gegen
  die aktuelle OpenRouter-API zu verifizieren. Fenster, die der Nutzer sperrt,
  fließen in den `TimeBlocked`-Filter ein. Fehlende Angaben (z. B. lokale
  Backends wie oMLX/Ollama) sind kein Fehler — Spalte bleibt leer.

## Design

```
Config:
  [backends.<name>].blocked_windows        → manuelle Backend-Sperren (R1)
  [registry.time_blocks.<model_id>]        → aus UI-Toggle persistierte Modell-Sperren (R3)

Registry (router-providers):
  openrouter.rs: /models → pricing.overrides → ModelCandidate.pricing_windows

Filter (router-core/rules.rs):
  now_utc (HH:MM) in blocked_windows ∪ registry.time_blocks[model_id]
    → FilterReason::TimeBlocked

Kosten-Score: unverändert — es wird nur ausgeschlossen, nicht umgerechnet.
Im aktiven Fenster gelten die Preise des aktiven Fensters (OpenRouter liefert
die Preise bereits fensterabhängig; sobald der Fetch die Fenster kennt, kann
der Cost-Term später optional das aktive Fenster-Preispaar nutzen — bewusst
NICHT Teil dieses CR).
```

## Betroffene Dateien

| Datei | Änderung |
|---|---|
| `crates/router-config/src/lib.rs` | `BackendConfig.blocked_windows`, `RegistryConfig.time_blocks` |
| `crates/router-core/src/rules.rs` | Filter `TimeBlocked` (UTC, Wrap-around) |
| `crates/router-core/src/registry.rs` | `ModelCandidate.pricing_windows` |
| `crates/router-providers/src/openrouter.rs` | overrides-Parsing |
| `crates/router-api/src/debug.rs` | `admin_config_get`: backends + time_blocks |
| `crates/router-api/src/ui/index.html` | Settings-Backends-Editor, Models-Spalte + Toggle |
| `config/router.toml`, `config/router.docker.toml` | Beispiel-Konfiguration + Kommentar |

## Tests

- `rules`: Fenster aktiv/inaktiv, Wrap-around (z. B. `22:00–06:00`), Grenzen
  (start == now, end == now), UTC-Basis.
- `openrouter`: overrides-Parsing (24h-Tiling, fehlende Angaben).
- `config`: `blocked_windows`/`time_blocks` parsen + serialisieren.
- UI: Smoke-Test Backend-Editor-Save (bestehender Save-&-restart-Flow).

## Offene Punkte

1. **Zeitzone:** UTC fix (Empfehlung) oder pro Fenster konfigurierbar
   (`tz = "Europe/Berlin"`)? Empfehlung: erst UTC, TZ-Feld nur wenn gebraucht.
2. **Wochentage** (z. B. nur Wochenende sperren): erstmal täglich, erweiterbar.
3. **Exakte OpenRouter-Feldnamen** der `pricing.overrides` im Implementierungs­
   schritt verifizieren (Doku: Array deckt immer den vollen Tag ab).
4. **Verhalten bei Überschneidung** von Backend-Sperre und Modell-Sperre:
   UND-Verknüpfung (eine reicht zum Sperren) — vorgeschlagen.
