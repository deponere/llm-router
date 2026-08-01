#!/usr/bin/env bash
# Speichert einen API-Key sicher in der macOS-Keychain.
# Der Key wird per stdin gelesen (read -s) → landet weder in Shell-History
# noch in einer Datei. Anzeige nur als Länge.
#
# Usage: store-key.sh [keychain-service-name]
#   Default-Service: openrouter-management-key (für rotate-openrouter-key.sh)
#
# Beispiel:
#   ./scripts/store-key.sh                 # Management-Key speichern
#   ./scripts/store-key.sh deepseek-key    # beliebiger weiterer Key
set -euo pipefail

service="${1:-openrouter-management-key}"

if security find-generic-password -a "$USER" -s "$service" -w >/dev/null 2>&1; then
    echo "⚠ Eintrag '$service' existiert bereits — wird mit dem neuen Key überschrieben (Strg-C zum Abbrechen)." >&2
fi

echo -n "Key für '$service' eingeben (Eingabe wird nicht angezeigt): " >&2
read -r -s key || true   # EOF (z. B. per Pipe) ist ok — der Leer-Check unten entscheidet
echo >&2

[ -n "$key" ] || { echo "Abbruch: leere Eingabe." >&2; exit 1; }

security add-generic-password -U -a "$USER" -s "$service" -w "$key"

len="$(security find-generic-password -a "$USER" -s "$service" -w | wc -c | tr -d ' ')"
echo "✓ Gespeichert in Keychain: '$service' (${len} Zeichen). Der Key wird nie wieder angezeigt." >&2
