#!/usr/bin/env bash
# OpenRouter-Key-Rotation: erzeugt einen neuen Inference-Key über das
# Management-API (/api/v1/keys), schreibt ihn in .env, löscht den alten Key.
#
# Konfiguration über .env (siehe .env.example):
#   OPENROUTER_LIMIT             USD-Limit pro Key (Pflicht, z. B. 50)
#   OPENROUTER_LIMIT_RESET       daily|weekly|monthly (optional, best-effort via update-API)
#   OPENROUTER_ROTATE_DAYS       Rotationsintervall (Default 90)
#   OPENROUTER_MGMT_KEY_SERVICE  Keychain-Service des Management-Keys (Default openrouter-management-key)
#   OPENROUTER_LAST_ROTATION     Unix-TS, wird vom Script gesetzt
#   OPENROUTER_KEY_HASH          Hash des aktuellen Keys, wird vom Script gesetzt
#
# Usage:
#   rotate-openrouter-key.sh --status   Konfig + Fälligkeit anzeigen (keine API-Calls)
#   rotate-openrouter-key.sh --check    Exit 0 = nicht fällig, 1 = fällig, 2 = Konfig-Fehler
#   rotate-openrouter-key.sh --auto     Rotiert nur, wenn fällig; still wenn nicht (Cron-Modus)
#   rotate-openrouter-key.sh --force    Rotiert sofort
#   rotate-openrouter-key.sh --dry-run  Prüft Voraussetzungen, tut nichts
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="${ROUTER_ENV_FILE:-$REPO_ROOT/.env}"
API="https://openrouter.ai/api/v1"
MGMT_SERVICE="${OPENROUTER_MGMT_KEY_SERVICE:-openrouter-management-key}"
ROTATE_DAYS="${OPENROUTER_ROTATE_DAYS:-90}"
NAME_PREFIX="router-"

say()  { printf '%s\n' "$*"; }
die()  { printf 'FEHLER: %s\n' "$*" >&2; exit 2; }

# --- .env lesen (nur die OPENROUTER_*-Felder, Werte nie ausgeben) ------------
env_get() { grep -E "^$1=" "$ENV_FILE" 2>/dev/null | head -1 | cut -d= -f2- || true; }

CURRENT_KEY="$(env_get OPENROUTER_API_KEY)"
LIMIT="$(env_get OPENROUTER_LIMIT)"
LIMIT_RESET="$(env_get OPENROUTER_LIMIT_RESET)"
LAST_ROTATION="$(env_get OPENROUTER_LAST_ROTATION)"
KEY_HASH="$(env_get OPENROUTER_KEY_HASH)"

[ -f "$ENV_FILE" ] || die ".env nicht gefunden: $ENV_FILE (ROUTER_ENV_FILE setzen?)"

# --- Management-Key aus der Keychain ----------------------------------------
mgmt_key() {
    security find-generic-password -a "$USER" -s "$MGMT_SERVICE" -w 2>/dev/null || true
}

mgmt_present() {
    security find-generic-password -a "$USER" -s "$MGMT_SERVICE" -w >/dev/null 2>&1
}

# --- JSON-Helfer (python3, überall auf macOS vorhanden) ----------------------
json_get() { python3 -c "import json,sys; d=json.load(sys.stdin); print(d$1 if d$1 is not None else '')" 2>/dev/null || true; }

curl_json() { # METHOD path [data] -> stdout: "HTTP_CODE<TAB>BODY"
    local method="$1" path="$2" data="${3:-}"
    local code body tmp
    tmp="$(mktemp)"
    if [ -n "$data" ]; then
        code="$(curl -sS -o "$tmp" -w '%{http_code}' --max-time 30 \
            -X "$method" "$API$path" \
            -H "Authorization: Bearer $MGMT" -H 'Content-Type: application/json' \
            -d "$data")"
    else
        code="$(curl -sS -o "$tmp" -w '%{http_code}' --max-time 30 \
            -X "$method" "$API$path" \
            -H "Authorization: Bearer $MGMT")"
    fi
    body="$(cat "$tmp")"; rm -f "$tmp"
    printf '%s\t%s\n' "$code" "$body"
}

# --- .env atomar aktualisieren (Temp-Datei + rename) --------------------------
env_set() { # name value — legt an oder ersetzt die Zeile
    python3 - "$ENV_FILE" "$1" "$2" <<'PY'
import os, sys, tempfile
path, name, val = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    lines = f.read().splitlines()
out, found = [], False
for ln in lines:
    if ln.startswith(name + "="):
        out.append(f"{name}={val}"); found = True
    else:
        out.append(ln)
if not found:
    out.append(f"{name}={val}")
fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path) or ".")
with os.fdopen(fd, "w") as f:
    f.write("\n".join(out) + "\n")
os.chmod(tmp, 0o600)
os.replace(tmp, path)
PY
}

# --- Anzeige ohne Secrets -----------------------------------------------------
show_status() {
    local now due_left
    now="$(date +%s)"
    say "OpenRouter-Rotation — Status"
    say "  .env:            $ENV_FILE"
    if mgmt_present; then say "  Management-Key:  $MGMT_SERVICE (in Keychain ✓)"; else say "  Management-Key:  $MGMT_SERVICE (FEHLT — ./scripts/store-key.sh ausführen)"; fi
    if [ -n "$LIMIT" ]; then say "  Limit:           \$$LIMIT / Mtok-Budget je Key"; else say "  Limit:           (nicht gesetzt!)"; fi
    [ -n "$LIMIT_RESET" ] && say "  Limit-Reset:     $LIMIT_RESET"
    say "  Intervall:       alle ${ROTATE_DAYS} Tage"
    if [ -n "$LAST_ROTATION" ]; then
        due_left=$((LAST_ROTATION + ROTATE_DAYS * 86400 - now))
        if [ "$due_left" -gt 0 ]; then
            say "  Letzte Rotation: $(date -r "$LAST_ROTATION" '+%F %H:%M') (in $((due_left / 86400)) Tagen fällig)"
        else
            say "  Letzte Rotation: $(date -r "$LAST_ROTATION" '+%F %H:%M') (fällig seit $((-due_left / 86400)) Tagen)"
        fi
    else
        say "  Letzte Rotation: (nie — erste Rotation fällig)"
    fi
    [ -n "$KEY_HASH" ] && say "  Aktueller Key:   Hash $KEY_HASH"
    return 0
}

is_due() {
    [ -z "$LAST_ROTATION" ] && return 0
    [ "$((LAST_ROTATION + ROTATE_DAYS * 86400))" -le "$(date +%s)" ]
}

rotate() {
    local mgmt new_resp code body new_key new_hash name limit_payload data
    mgmt="$(mgmt_key)"
    [ -n "$mgmt" ] || die "Management-Key fehlt in Keychain ('$MGMT_SERVICE'). Erst: ./scripts/store-key.sh"

    if [ -z "$LIMIT" ]; then
        say "⚠ OPENROUTER_LIMIT ist in .env nicht gesetzt — Key wird OHNE Limit erzeugt." >&2
    fi
    name="${NAME_PREFIX}$(date +%Y%m%d-%H%M)"

    # 1. Neuen Key erzeugen
    if [ -n "$LIMIT" ]; then
        data="$(python3 -c "import json,sys; print(json.dumps({'name': sys.argv[1], 'limit': float(sys.argv[2])}))" "$name" "$LIMIT")"
    else
        data="$(python3 -c "import json,sys; print(json.dumps({'name': sys.argv[1]}))" "$name")"
    fi
    new_resp="$(curl_json POST /keys "$data")"
    code="${new_resp%%$'\t'*}"; body="${new_resp#*$'\t'}"
    if [ "$code" != "200" ]; then
        die "Key-Erzeugung fehlgeschlagen (HTTP $code): $(printf '%s' "$body" | head -c 300)"
    fi
    new_key="$(printf '%s' "$body" | json_get "['data']['key']")"
    new_hash="$(printf '%s' "$body" | json_get "['data']['id']")"
    [ -n "$new_key" ] || die "Antwort ohne key-Feld: $(printf '%s' "$body" | head -c 200)"

    # 2. Neuen Key verifizieren (kann selbst chatten)
    if ! curl -sS -o /dev/null --max-time 15 "https://openrouter.ai/api/v1/auth/key" -H "Authorization: Bearer $new_key"; then
        say "⚠ Neuer Key konnte nicht verifiziert werden — Hash $new_hash bitte manuell prüfen/löschen." >&2
    fi

    # 3. Optional Limit-Reset setzen (best-effort: PATCH, sonst PUT)
    if [ -n "$LIMIT_RESET" ] && [ -n "$new_hash" ]; then
        local up code2
        up="$(curl_json PATCH "/keys/$new_hash" "{\"limit_reset\": \"$LIMIT_RESET\"}")"
        code2="${up%%$'\t'*}"
        if [ "$code2" != "200" ] && [ "$code2" != "204" ]; then
            up="$(curl_json PUT "/keys/$new_hash" "{\"limit_reset\": \"$LIMIT_RESET\"}")"
            code2="${up%%$'\t'*}"
        fi
        [ "$code2" = "200" ] || [ "$code2" = "204" ] \
            || say "⚠ limit_reset=$LIMIT_RESET konnte nicht gesetzt werden (HTTP $code2) — Dashboard prüfen." >&2
    fi

    # 4. .env atomar aktualisieren (Key + State)
    env_set OPENROUTER_API_KEY "$new_key"
    env_set OPENROUTER_LAST_ROTATION "$(date +%s)"
    [ -n "$new_hash" ] && env_set OPENROUTER_KEY_HASH "$new_hash"

    # 5. Alten Key löschen (Hash aus vorheriger Rotation, sonst Label-Heuristik)
    local old_hash=""
    if [ -n "$KEY_HASH" ]; then
        old_hash="$KEY_HASH"
    else
        local list code3
        list="$(curl_json GET /keys)"
        code3="${list%%$'\t'*}"
        if [ "$code3" = "200" ]; then
            old_hash="$(printf '%s' "${list#*$'\t'}" | python3 -c "
import json,sys
d=json.load(sys.stdin)
for k in d.get('data', []):
    if k.get('label','').startswith('$NAME_PREFIX') and k.get('id') != '$new_hash':
        print(k['id']); break
")"
        fi
    fi
    if [ -n "$old_hash" ]; then
        local del code4
        del="$(curl_json DELETE "/keys/$old_hash")"
        code4="${del%%$'\t'*}"
        if [ "$code4" = "200" ] || [ "$code4" = "204" ]; then
            say "✓ Alter Key gelöscht (Hash $old_hash)"
        else
            say "⚠ Alter Key konnte nicht gelöscht werden (HTTP $code4) — Hash $old_hash manuell entfernen." >&2
        fi
    else
        say "⚠ Kein alter Key identifiziert (OPENROUTER_KEY_HASH leer, kein '${NAME_PREFIX}'*-Label) — alten Key im Dashboard prüfen." >&2
    fi

    say "✓ Rotation abgeschlossen: $name (Hash $new_hash, Limit ${LIMIT:-keins}${LIMIT_RESET:+, Reset $LIMIT_RESET})"
    say "  Router neu starten, damit der neue Key geladen wird (Admin-App → Router → Restart)."
}

# --- Main ---------------------------------------------------------------------
cmd="${1:---status}"
case "$cmd" in
    --status)
        show_status
        ;;
    --check)
        mgmt_present || die "Management-Key fehlt in Keychain ('$MGMT_SERVICE'). Erst: ./scripts/store-key.sh"
        if is_due; then say "fällig"; exit 1; else say "nicht fällig"; exit 0; fi
        ;;
    --auto)
        mgmt_present || die "Management-Key fehlt in Keychain ('$MGMT_SERVICE'). Erst: ./scripts/store-key.sh"
        if is_due; then rotate; else exit 0; fi
        ;;
    --force)
        rotate
        ;;
    --dry-run)
        show_status
        mgmt_present || die "Management-Key fehlt in Keychain ('$MGMT_SERVICE'). Erst: ./scripts/store-key.sh"
        say "  Dry-Run: würde Key '$NAME_PREFIX$(date +%Y%m%d-%H%M)' mit Limit ${LIMIT:-<kein>} erzeugen und .env aktualisieren."
        ;;
    *)
        say "Usage: $(basename "$0") [--status|--check|--auto|--force|--dry-run]" >&2
        exit 2
        ;;
esac
