#!/bin/sh
# Rebuilds the development database from the migrations, keeping its data.
#
# The migrations are edited in place until there is a release, so an edited
# schema cannot be applied to the database already there. This dumps every
# table's rows, drops the schema, migrates afresh, and loads the rows back
# before the server starts -- so users, sessions, templates, environments and
# worker credentials all survive, and nobody is signed out.
#
# Rows are loaded as INSERTs naming their columns, so a table that has gained
# a column takes its default. Rows that no longer fit -- a column dropped or
# renamed, a new NOT NULL with no default -- are skipped, and the errors
# printed, rather than stopping the rest.
#
# Run from the repository root, with the stack up: dev/reset-db.sh
set -eu

psql() { docker compose exec -T postgres psql -U hangar -d hangar -q "$@"; }
dump=$(mktemp)
trap 'rm -f "$dump"' EXIT

echo "reset-db: stopping the server and worker"
docker compose stop server worker > /dev/null

echo "reset-db: saving the data"
docker compose exec -T postgres pg_dump -U hangar -d hangar \
    --data-only --column-inserts --exclude-table=schema_migrations > "$dump"
echo "reset-db: $(grep -c '^INSERT' "$dump" || true) rows saved"

echo "reset-db: rebuilding the schema"
psql -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;' 2>&1 | grep -v 'NOTICE\|DETAIL\|drop cascades' || true
docker compose run --rm -T server go run ./cmd/hangar-server -migrate-only 2>&1 | grep -v '^go: ' || true

echo "reset-db: loading the data back"
# Not in one transaction: a row that no longer fits is skipped on its own.
failed=$(psql -v ON_ERROR_STOP=0 < "$dump" 2>&1 >/dev/null | grep 'ERROR' || true)
if [ -n "$failed" ]; then
    echo "reset-db: $(printf '%s\n' "$failed" | wc -l | tr -d ' ') rows did not fit and were skipped:"
    printf '%s\n' "$failed" | sed 's/^psql:<stdin>:[0-9]*: //' | sort | uniq -c | sort -rn | head -20
fi

echo "reset-db: starting the server and worker"
docker compose start server worker > /dev/null
echo "reset-db: done"
