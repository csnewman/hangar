#!/bin/sh
# Rebuilds the development database from the migrations, keeping its data.
#
# The migrations are edited in place until there is a release, so an edited
# schema cannot be applied to the database already there. This builds a new
# database beside it from the migrations, copies every row across as INSERTs
# naming their columns -- so a table that has gained a column takes its
# default -- and only then swaps the new database in, keeping the old one as
# hangar_prev until the next run. To go back to it:
#
#   docker compose exec postgres psql -U hangar -d postgres \
#     -c 'ALTER DATABASE hangar RENAME TO hangar_bad' \
#     -c 'ALTER DATABASE hangar_prev WITH ALLOW_CONNECTIONS true' \
#     -c 'ALTER DATABASE hangar_prev RENAME TO hangar'
#
# The new database is built under another name so that nothing else can
# write to it meanwhile: compose's watch restarts the server whenever Go
# files change, stopped or not, and a server that found an empty schema
# would create its first administrator, whose username then refuses the
# real one and every row that belongs to it.
#
# A row that does not fit the new schema -- a column dropped or renamed, a
# new NOT NULL with no default -- is reported, and nothing is swapped: the
# database in use is left as it was. RESET_DB_DROP_UNFIT=1 swaps anyway,
# losing those rows.
#
# Run from the repository root, with the stack up: dev/reset-db.sh
set -eu

psql() { docker compose exec -T postgres psql -U hangar -q "$@"; }
dump=dev/.reset-db-dump.sql

echo "reset-db: copying the data out"
docker compose exec -T postgres pg_dump -U hangar -d hangar \
    --data-only --column-inserts --exclude-table=schema_migrations > "$dump"
echo "reset-db: $(grep -c '^INSERT' "$dump" || true) rows (kept in $dump)"

echo "reset-db: building the new schema"
psql -d postgres -c 'DROP DATABASE IF EXISTS hangar_new WITH (FORCE)' -c 'CREATE DATABASE hangar_new'
docker compose run --rm -T \
    -e HANGAR_DATABASE_URL='postgres://hangar:hangar@postgres:5432/hangar_new?sslmode=disable' \
    server go run ./cmd/hangar-server -migrate-only 2>&1 | grep -v '^go: \|Container' || true

echo "reset-db: loading the data into it"
failed=$(psql -d hangar_new -v ON_ERROR_STOP=0 < "$dump" 2>&1 >/dev/null | grep 'ERROR' || true)
if [ -n "$failed" ]; then
    echo "reset-db: $(printf '%s\n' "$failed" | wc -l | tr -d ' ') rows do not fit the new schema:"
    printf '%s\n' "$failed" | sed 's/^psql:<stdin>:[0-9]*: /    /' | sort | uniq -c | sort -rn | head -20
    if [ "${RESET_DB_DROP_UNFIT:-}" != 1 ]; then
        echo "reset-db: nothing changed; fix the migration, or run with RESET_DB_DROP_UNFIT=1 to lose them"
        exit 1
    fi
fi

echo "reset-db: swapping it in (the old one is hangar_prev)"
docker compose stop server worker > /dev/null
# Connections are refused before the rest are ended, so none can arrive
# between that and the rename.
psql -d postgres -c 'DROP DATABASE IF EXISTS hangar_prev WITH (FORCE)' \
    -c 'ALTER DATABASE hangar WITH ALLOW_CONNECTIONS false' \
    -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'hangar'" \
    -c 'ALTER DATABASE hangar RENAME TO hangar_prev' \
    -c 'ALTER DATABASE hangar_new RENAME TO hangar' > /dev/null

echo "reset-db: starting the server and worker"
docker compose start server worker > /dev/null
echo "reset-db: done"
