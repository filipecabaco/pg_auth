#!/bin/sh
# Run once by the Postgres image when its cluster is made: the demo's roles, tables and rules
# (docker/demo/sql), the sidecar's login, and pg_auth pointed at the sidecar's URL.
set -e
run() { psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" "$@"; }
run -f /demo/sql/roles.sql
run -v engine=spicedb -v rules_name=schema -v rules_file=/demo/rules/spicedb.zed -f /demo/sql/relationships.sql
run <<'SQL'
-- the sidecar's login: the functions it calls, nothing else (authz_sidecar)
create role sidecar login password 'sidecar' in role authz_sidecar;
alter system set pg_auth.sidecar_url = 'https://sidecar:8443';
alter system set pg_auth.sidecar_secret_file = '/etc/pg_auth/secret';
alter system set pg_auth.sidecar_ca_file = '/etc/pg_auth/ca.pem';
SQL
