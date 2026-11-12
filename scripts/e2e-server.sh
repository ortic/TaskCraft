#!/usr/bin/env bash
# Starts TaskCraft for the end-to-end tests on a fresh database (E2E_DB, default taskcraft_e2e).
# Needs Postgres (TASKCRAFT_E2E_ADMIN_URL, default: the dev container on 127.0.0.1:55433)
# and a built frontend (web/dist). E-mail goes to E2E_SMTP_URL (the tests' mail sink) when set.
set -euo pipefail
cd "$(dirname "$0")/.."
PORT="${E2E_PORT:-8798}"
DB="${E2E_DB:-taskcraft_e2e}"
[[ "$DB" =~ ^[a-z0-9_]+$ ]] || { echo "E2E_DB: letters, digits and _ only" >&2; exit 2; }
ADMIN_URL="${TASKCRAFT_E2E_ADMIN_URL:-postgres://taskcraft:taskcraft@127.0.0.1:55433/postgres}"
DB_URL="${ADMIN_URL%/*}/$DB"
sql() {
  if command -v psql >/dev/null; then psql "$ADMIN_URL" -qc "$1"
  else docker exec taskcraft-pg psql -U taskcraft -d postgres -qc "$1"; fi
}
sql "DROP DATABASE IF EXISTS $DB WITH (FORCE)"
sql "CREATE DATABASE $DB"
[ -f web/dist/index.html ] || (cd web && npm ci && npm run build)
DATA="$(mktemp -d)"
trap 'rm -rf "$DATA"' EXIT
exec env TASKCRAFT_DATABASE_URL="$DB_URL" TASKCRAFT_BIND="127.0.0.1:$PORT" TASKCRAFT_BASE_URL="http://127.0.0.1:$PORT" \
  TASKCRAFT_DATA_DIR="$DATA" TASKCRAFT_STATIC_DIR=web/dist RUST_LOG="${RUST_LOG:-warn}" \
  TASKCRAFT_SMTP_URL="${E2E_SMTP_URL:-}" TASKCRAFT_MAIL_FROM="TaskCraft <tasks@example.com>" \
  cargo run -q -p taskcraft -- serve
