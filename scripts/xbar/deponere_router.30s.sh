#!/usr/bin/env bash
# <xbar.title>deponere_router</xbar.title>
# <xbar.version>v0.1</xbar.version>
# <xbar.author>map</xbar.author>
# <xbar.desc>Zeigt Kosten und letzte LLM-Aufrufe vom lokalen deponere_router.</xbar.desc>
# <xbar.dependencies>jq</xbar.dependencies>
#
# Installation:
#   1. xbar installieren (https://xbarapp.com) oder `brew install --cask xbar`
#   2. Dieses Skript nach ~/Library/Application\ Support/xbar/plugins/ kopieren
#      (xbar > Open Plugin Folder in der Menubar)
#   3. chmod +x deponere_router.30s.sh
#   4. xbar > Refresh All
#
# Der Dateiname endet auf `.30s.sh` = Refresh alle 30 Sekunden. Aendere das
# Intervall direkt im Dateinamen (z.B. `.10s.sh` oder `.1m.sh`).

set -euo pipefail

# xbar startet das Skript mit der System-Locale (z. B. de_DE), wo das
# Dezimaltrennzeichen Komma ist. printf '%f' rejectet dann Werte wie "0.0".
# Mit C-Locale bleibt der Punkt als Trennzeichen.
export LC_ALL=C

ROUTER="${ROUTER_URL:-http://127.0.0.1:4000}"
ICON_OFF="💤"
ICON_ON="💸"

JQ_BIN="$(command -v jq || true)"
if [[ -z "$JQ_BIN" ]]; then
    echo "deponere_router (jq missing)"
    echo "---"
    echo "Install jq: brew install jq | refresh=true"
    exit 0
fi

# Kurzer Connect-Check, sonst "offline" anzeigen.
if ! curl -fsS -m 2 "$ROUTER/healthz" >/dev/null 2>&1; then
    echo "$ICON_OFF router offline"
    echo "---"
    echo "Router nicht erreichbar unter $ROUTER"
    echo "Starten: cd ~/dev/router && cargo run -p router-api | bash=cargo param1=run param2=-p param3=router-api terminal=true"
    exit 0
fi

RESP="$(curl -fsS -m 3 "$ROUTER/v1/transactions?limit=10")"

TODAY_COST=$(echo "$RESP" | jq -r '.totals_today_utc.cost_usd // 0')
TODAY_COUNT=$(echo "$RESP" | jq -r '.totals_today_utc.count // 0')
SESSION_COST=$(echo "$RESP" | jq -r '.totals_session.cost_usd // 0')
SESSION_COUNT=$(echo "$RESP" | jq -r '.totals_session.count // 0')
SESSION_START=$(echo "$RESP" | jq -r '.session_start_unix // 0')

# Menubar-Kopfzeile: kumulierte Kosten heute.
printf '%s $%.4f · %d\n' "$ICON_ON" "$TODAY_COST" "$TODAY_COUNT"

echo "---"

# Session-Zusammenfassung
if [[ "$SESSION_START" -gt 0 ]]; then
    SESSION_START_HUMAN=$(date -r "$SESSION_START" '+%H:%M')
    printf 'Session seit %s: $%.4f · %d calls | color=gray\n' "$SESSION_START_HUMAN" "$SESSION_COST" "$SESSION_COUNT"
fi
printf 'Heute (UTC): $%.4f · %d calls | color=gray\n' "$TODAY_COST" "$TODAY_COUNT"

echo "---"
echo "Letzte Aufrufe | color=gray"

echo "$RESP" | jq -r '
  .recent[] |
  (.unix_ts | strftime("%H:%M:%S")) + " | " +
  (.backend | if . == "OMlx" then "🖥️ " else "☁️ " end) +
  .model_id + " | " +
  (if .cost_usd == null then "—" else "$" + (.cost_usd | tostring) end) + " | " +
  ((.duration_ms | tostring) + "ms")
' | while IFS='|' read -r ts backend_model cost duration; do
    # Zeile formatieren; font=Menlo = monospaced, damit Spalten alignen
    printf '%s %s %s %s | font=Menlo size=12\n' \
        "$(echo "$ts" | xargs)" \
        "$(echo "$backend_model" | xargs)" \
        "$(echo "$cost" | xargs)" \
        "$(echo "$duration" | xargs)"
done

echo "---"
printf 'Router UI (Registry) | href=%s/v1/registry color=blue\n' "$ROUTER"
printf 'Raw JSON | href=%s/v1/transactions color=blue\n' "$ROUTER"
echo "Refresh | refresh=true"
