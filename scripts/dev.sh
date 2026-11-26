#!/usr/bin/env bash
# Local runner for learning and verifying the Content Agent.
#
#   ./scripts/dev.sh start        build + start Postgres, fake Zernio, API and worker
#   ./scripts/dev.sh demo         guided walkthrough that explains every step as it runs
#   ./scripts/dev.sh test         run the automated test suite
#   ./scripts/dev.sh status       what is running, and where
#   ./scripts/dev.sh logs [api|worker|zernio]
#   ./scripts/dev.sh stop [--clean]   stop everything (--clean also deletes the dev database)
#   ./scripts/dev.sh help
#
# Everything here is LOCAL and for development: mock LLM + mock images (no API keys), a throwaway Postgres in Docker,
# and a fake Zernio. To try a real LLM instead:  LLM_PROVIDER=anthropic LLM_API_KEY=... ./scripts/dev.sh start
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
STATE="$ROOT/.dev"
mkdir -p "$STATE"
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

API_PORT="${API_PORT:-8787}"
ZERNIO_PORT="${ZERNIO_PORT:-18091}"
PG_PORT="${PG_PORT:-55434}"
PG_NAME="content-agent-dev-pg"
export DEV_SECRET="${DEV_SECRET:-dev-secret-dev-secret-dev-secret-1234}"  # dev only; production secrets come from a secret manager
export API_BASE="http://127.0.0.1:${API_PORT}"

if [ -t 1 ]; then B=$'\033[1m'; D=$'\033[2m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; P=$'\033[35m'; N=$'\033[0m'; else B=""; D=""; G=""; Y=""; R=""; P=""; N=""; fi
say()  { printf '%s\n' "$*"; }
step() { printf '\n%s==>%s %s%s%s\n' "$P" "$N" "$B" "$*" "$N"; }
ok()   { printf '  %s✓%s %s\n' "$G" "$N" "$*"; }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*"; }
die()  { printf '%s✗ %s%s\n' "$R" "$*" "$N" >&2; exit 1; }

pidfile() { printf '%s/%s.pid' "${STATE:?}" "${1:?}"; }
alive() { local f; f="$(pidfile "$1")"; [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null; }

launch() { # name, command...
  local name="$1"; shift
  if alive "$name"; then ok "$name already running (pid $(cat "$(pidfile "$name")"))"; return; fi
  setsid nohup "$@" >"$STATE/$name.log" 2>&1 < /dev/null &
  echo $! >"$(pidfile "$name")"
}

wait_http() { # url, seconds
  local i; for ((i = 0; i < $2 * 2; i++)); do curl -fsS "$1" >/dev/null 2>&1 && return 0; sleep 0.5; done; return 1
}

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required. $2"; }

cmd_start() {
  step "1/5  Checking prerequisites"
  need cargo "Install Rust: https://rustup.rs"
  need python3 "Needed for the fake Zernio and the walkthrough."
  need curl "Needed for health checks."
  ok "cargo $(cargo --version | cut -d' ' -f2), python3 $(python3 --version | cut -d' ' -f2)"

  step "2/5  Database (Postgres holds jobs, drafts, audit trail AND the work queue)"
  if [ -n "${DATABASE_URL:-}" ]; then
    ok "using DATABASE_URL from your environment"
  else
    need docker "Install Docker, or set DATABASE_URL to a Postgres you already have."
    if docker ps --format '{{.Names}}' | grep -qx "$PG_NAME"; then
      ok "dev Postgres already running on port $PG_PORT"
    else
      docker rm -f "$PG_NAME" >/dev/null 2>&1 || true
      docker run -d --name "$PG_NAME" -e POSTGRES_PASSWORD=dev -e POSTGRES_DB=content_agent -p "127.0.0.1:${PG_PORT}:5432" postgres:16-alpine >/dev/null
      local i; for ((i = 0; i < 60; i++)); do docker exec "$PG_NAME" pg_isready -U postgres >/dev/null 2>&1 && break; sleep 0.5; done
      ok "started throwaway Postgres 16 on 127.0.0.1:$PG_PORT"
    fi
    export DATABASE_URL="postgres://postgres:dev@127.0.0.1:${PG_PORT}/content_agent"
  fi

  step "3/5  Building (first build takes a few minutes; later ones are seconds)"
  cargo build --bins 2>&1 | tail -n 3
  ok "built content-agent-api and content-agent-worker"

  step "4/5  Starting services"
  launch zernio python3 "$ROOT/scripts/mock_zernio.py" "$ZERNIO_PORT"
  local envs=(
    APP_ENV=development
    "DATABASE_URL=$DATABASE_URL"
    "BIND_ADDR=127.0.0.1:${API_PORT}"
    "INTERNAL_AUTH_SECRETS=$DEV_SECRET"
    "LLM_PROVIDER=${LLM_PROVIDER:-mock}"
    "LLM_API_KEY=${LLM_API_KEY:-}"
    "LLM_MODEL=${LLM_MODEL:-claude-sonnet-5-5}"
    IMAGE_PROVIDER=mock
    STORAGE_KIND=local
    "LOCAL_STORAGE_DIR=$STATE/assets"
    "ZERNIO_BASE_URL=http://127.0.0.1:${ZERNIO_PORT}"
    ZERNIO_API_KEY=dev-mock-key
    CONSOLE_ENABLED=true
    "RUST_LOG=${RUST_LOG:-info}"
  )
  launch api env "${envs[@]}" "$ROOT/target/debug/content-agent-api"
  launch worker env "${envs[@]}" "$ROOT/target/debug/content-agent-worker"

  step "5/5  Waiting for the API to report ready"
  if wait_http "$API_BASE/readyz" 30; then ok "API is ready: $(curl -fsS "$API_BASE/readyz")"
  else say "--- last API log lines ---"; tail -n 20 "$STATE/api.log" || true; die "API did not become ready. See: ./scripts/dev.sh logs api"; fi

  cat <<EOF

${B}Everything is running.${N}  Try these, in this order:

  ${B}1. Guided walkthrough${N} (explains each step as it runs)
       ./scripts/dev.sh demo

  ${B}2. Developer console${N} (click through the tools yourself)
       ${API_BASE}/console
       signing secret:  ${DEV_SECRET}
       tip: run ingest_url, then extract_content, generate_content_report, ... in the sidebar order.

  ${B}3. Look under the hood${N}
       API contract:  docs/openapi.json and docs/TOOLS.md
       Metrics:       ${API_BASE}/metrics
       Database:      docker exec -it ${PG_NAME} psql -U postgres content_agent -c "select entity_kind, from_state, to_state, reason from state_transitions order by at desc limit 10;"
       Fake Zernio:   ./scripts/dev.sh logs zernio

  ${B}4. Run the automated tests${N}      ./scripts/dev.sh test
  ${B}5. Stop${N}                         ./scripts/dev.sh stop        (add --clean to delete the dev database)

${B}Optional keys${N} (export before 'start' to unlock more; nothing below is required):
       X_BEARER_TOKEN=...     read X (Twitter) posts          YOUTUBE_API_KEY=...   read YouTube videos
       GITHUB_TOKEN=...       private repos / higher limits   FIRECRAWL_API_KEY=... JavaScript-heavy pages
       LLM_PROVIDER=anthropic LLM_API_KEY=...                 real AI wording instead of the offline mock
       example:  X_BEARER_TOKEN=xxxx ./scripts/dev.sh stop && X_BEARER_TOKEN=xxxx ./scripts/dev.sh start

Read ${B}docs/LEARN.md${N} for what each piece is and why it exists.
EOF
}

cmd_demo() {
  curl -fsS "$API_BASE/readyz" >/dev/null 2>&1 || die "The API is not running. Start it first: ./scripts/dev.sh start"
  exec python3 "$ROOT/scripts/walkthrough.py" "$@"
}

cmd_test() {
  need cargo "Install Rust: https://rustup.rs"
  step "Running the automated tests (unit + integration + contract, in-memory store)"
  cargo test
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "$PG_NAME"; then
    step "Running the Postgres integration tests against the dev database"
    TEST_DATABASE_URL="postgres://postgres:dev@127.0.0.1:${PG_PORT}/content_agent" cargo test --test postgres
  else
    warn "dev Postgres is not running, so the Postgres tests were skipped (run ./scripts/dev.sh start first to include them)"
  fi
}

cmd_status() {
  local n
  for n in zernio api worker; do
    if alive "$n"; then ok "$n running (pid $(cat "$(pidfile "$n")"))"; else warn "$n not running"; fi
  done
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "$PG_NAME"; then ok "postgres container $PG_NAME on port $PG_PORT"; else warn "dev postgres container not running"; fi
  if curl -fsS "$API_BASE/readyz" 2>/dev/null; then echo; else warn "API not answering on $API_BASE"; fi
}

cmd_logs() {
  local n="${1:-api}"; [ -f "$STATE/$n.log" ] || die "no log for '$n' (choose api, worker or zernio)"
  say "${D}(Ctrl-C to stop following)${N}"; tail -n 40 -f "$STATE/$n.log"
}

cmd_stop() {
  local n f
  for n in api worker zernio; do
    f="$(pidfile "$n")"
    if alive "$n"; then kill "$(cat "$f")" 2>/dev/null && ok "stopped $n"; fi
    [ -f "$f" ] && rm -f -- "$f"
  done
  if [ "${1:-}" = "--clean" ]; then
    if docker rm -f "$PG_NAME" >/dev/null 2>&1; then ok "removed dev database"; fi
    # Remove only the files this script created inside .dev (never the directory by variable).
    rm -f -- "${STATE:?}"/api.log "${STATE:?}"/worker.log "${STATE:?}"/zernio.log
  else
    say "  (dev database kept; use '--clean' to delete it)"
  fi
}

case "${1:-start}" in
  start)  cmd_start ;;
  demo)   shift; cmd_demo "$@" ;;
  test)   cmd_test ;;
  status) cmd_status ;;
  logs)   shift; cmd_logs "${1:-api}" ;;
  stop)   shift; cmd_stop "${1:-}" ;;
  help|-h|--help) sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
  *) die "unknown command '$1' (try: ./scripts/dev.sh help)" ;;
esac
