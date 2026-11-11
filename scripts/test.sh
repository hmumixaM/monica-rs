#!/bin/sh
set -eu
# Always create an isolated local cluster; never read an existing .env.
MONICA_PG_BIN=${MONICA_PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}
MONICA_TEST_ROOT=$(mktemp -d /tmp/monica-rs-test.XXXXXX)
cleanup() {
  "$MONICA_PG_BIN/pg_ctl" -D "$MONICA_TEST_ROOT/db" -m immediate stop >/dev/null 2>&1 || true
  rm -rf "$MONICA_TEST_ROOT"
}
trap cleanup EXIT INT TERM
"$MONICA_PG_BIN/initdb" -D "$MONICA_TEST_ROOT/db" --auth=trust --no-locale --encoding=UTF8 >/dev/null
# Unix socket only. A unique directory avoids port races and outside clients.
"$MONICA_PG_BIN/pg_ctl" -D "$MONICA_TEST_ROOT/db" -l "$MONICA_TEST_ROOT/postgres.log" -o "-c listen_addresses='' -k $MONICA_TEST_ROOT -c max_connections=100" -w start >/dev/null
DATABASE_URL="postgresql:///postgres?host=$MONICA_TEST_ROOT" cargo test --workspace
cargo build -p monica-server --locked
export MONICA_DATABASE_URL="postgresql:///postgres?host=$MONICA_TEST_ROOT"
target/debug/monica-server migrate
python3 scripts/generate-fixtures.py --records 100 > "$MONICA_TEST_ROOT/fixtures.json"
target/debug/monica-server ingest "$MONICA_TEST_ROOT/fixtures.json"
target/debug/monica-server drain
MONICA_PROJECTED=$("$MONICA_PG_BIN/psql" "$MONICA_DATABASE_URL" -Atc 'SELECT count(*) FROM crm.projections')
test "$MONICA_PROJECTED" = 100
python3 scripts/smoke-http.py
