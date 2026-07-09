# Router Admin (macOS)

Menüleisten-App zum Bearbeiten von `config/router.toml` und zum Steuern des
Router-Prozesses — voll editierbar: Backends, Profile (Gewichte, Allow/Denylists,
Provider-Flags), Registry (Intelligence, Privacy, Overrides).

## Architektur

- **SwiftUI-Menüleisten-App** (`Sources/RouterAdmin/`) — reine JSON-Bridge, kein
  TOML in Swift.
- **`router-admin`** (Rust, `crates/router-admin/`) — `dump` liefert die Config als
  JSON, `apply` schreibt geändertes JSON **format-erhaltend** via `toml_edit`
  zurück in die TOML. Kommentare und Reihenfolge bleiben erhalten; vor jedem
  Schreiben wird `router.toml.bak` angelegt.

## Bauen

```bash
./build-app.sh          # -> RouterAdmin.app
open RouterAdmin.app     # Icon erscheint in der Menüleiste
```

Autostart: `Systemeinstellungen → Allgemein → Anmeldeobjekte → RouterAdmin.app`.

## Bedienung

- **Backends / Profile / Registry** — Felder bearbeiten, dann **Speichern** (⌘S).
- **Router** — Start / Stop / Neustart. Config-Änderungen wirken erst nach
  Neustart (kein Hot-Reload).
- Der Statuspunkt oben pollt `GET /v1/models` am konfigurierten Bind.

## Verifikation

```bash
swift build
.build/debug/RouterAdmin --selftest ../target/debug/router-admin ../config/router.toml
```

Testet die Kette dump → decode → edit → encode → apply → dump gegen eine
Wegwerf-Kopie und prüft Werte- und Kommentar-Erhalt.

## Bekannte Grenzen

- Der Router läuft als eigenständiger Prozess; nach Beenden der App läuft er
  weiter. Stop trifft ihn per `pkill` (Pattern endet auf `/router`).
- `router` / `router-admin` müssen als Release-Binary in `target/release/`
  liegen (macht `build-app.sh`).
