#!/bin/sh
# Ollama-Container-Entrypoint: Server starten, in OLLAMA_MODELS (kommagetrennt)
# gelistete Modelle beim Start laden, dann vordergründig laufen lassen.
# Wird als CMD-Override in docker-compose.yml verwendet.
set -e

ollama serve &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true' TERM INT

# Warten bis der Server antwortet (max. 60 s)
i=0
while ! ollama list >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -ge 60 ]; then
    echo "WARN: Ollama-Server antwortet nicht nach 60s — starte trotzdem weiter"
    break
  fi
  sleep 1
done

if [ -n "$OLLAMA_MODELS" ]; then
  for m in $(echo "$OLLAMA_MODELS" | tr ',' ' '); do
    [ -z "$m" ] && continue
    echo "ollama pull $m …"
    ollama pull "$m" || echo "WARN: pull '$m' fehlgeschlagen (später: docker compose exec ollama ollama pull $m)"
    # Warmup: Modell in den Speicher laden, damit der erste echte Request
    # nicht minutenlang auf den Load wartet (besonders ohne GPU).
    echo "ollama run $m (warmup) …"
    ollama run --keepalive -1 "$m" "hi" >/dev/null 2>&1 || echo "WARN: warmup '$m' fehlgeschlagen"
  done
fi

wait "$SERVER_PID"
