#!/bin/sh
# Postgres, the three engines and a sidecar for each, then tmux: admin on top, anne and bob side by
# side, the sidecars' log below. The snapshot is taken at the admin prompt, so visitors restore a
# machine that is already up. When tmux exits the console runs this again; everything is still up.
PSQL="psql -h /run/postgresql -U postgres -X -q -tA"
LOG=/run/pg_auth/sidecar.log

# Go's runtime, sized for a browser tab: one thread each, collected early
export GOMAXPROCS=1 GOGC=50 GOMEMLIMIT=96MiB

wait_for() { until curl -sf -o /dev/null "$@"; do sleep 0.3; done; }

# a sidecar per engine, as the postgres user (its socket is created 0660); its log lines tagged
# with the engine and appended to the log pane's file
sidecar() {
  name=$1; shift
  ( su-exec postgres env "$@" REQUEST_LOG=1 PG_AUTH_SOCKET=/run/pg_auth/$name.sock pg-auth-sidecar 2>&1 \
    | awk -v engine="$name" '{ sub(/^pg-auth-sidecar: /, ""); printf "%-8s %s\n", engine, $0; fflush() }' >> $LOG ) &
}

if [ ! -S /run/pg_auth/cerbos.sock ]; then
  install -d -o postgres -g postgres /run/postgresql /run/pg_auth
  su-exec postgres pg_ctl -D "$PGDATA" -l /run/postgresql/server.log -w -s start

  openfga run --datastore-engine memory --playground-enabled=false --metrics-enabled=false \
    --http-addr 127.0.0.1:8080 --grpc-addr 127.0.0.1:8081 --log-level error > /run/pg_auth/openfga.log 2>&1 &
  spicedb serve --grpc-preshared-key demo --datastore-engine memory --http-enabled \
    --http-addr 127.0.0.1:8443 --grpc-addr 127.0.0.1:50051 --metrics-enabled=false --telemetry-endpoint= \
    --log-level error > /run/pg_auth/spicedb.log 2>&1 &
  cerbos server --config=/etc/demo/cerbos/conf.yaml > /run/pg_auth/cerbos.log 2>&1 &

  # OpenFGA: a store and the model (the store id is new each boot, so the sidecar learns it here)
  wait_for http://127.0.0.1:8080/healthz
  store=$(curl -sf -X POST http://127.0.0.1:8080/stores -d '{"name": "demo"}' | jq -r .id)
  model=$(curl -sf -X POST "http://127.0.0.1:8080/stores/$store/authorization-models" -d @/etc/demo/model.json \
    | jq -r .authorization_model_id)
  # SpiceDB: the schema, once its HTTP gateway answers
  until curl -sf -o /dev/null -X POST http://127.0.0.1:8443/v1/schema/write \
    -H 'Authorization: Bearer demo' -d @/etc/demo/schema.json; do sleep 0.3; done
  wait_for http://127.0.0.1:3592/_cerbos/health

  : > $LOG
  sidecar openfga OPENFGA_URL=http://127.0.0.1:8080 OPENFGA_STORE_ID="$store" OPENFGA_MODEL_ID="$model" \
    PG_AUTH_DATABASE_URL="host=/run/postgresql user=postgres dbname=openfga"
  sidecar spicedb SPICEDB_URL=http://127.0.0.1:8443 SPICEDB_TOKEN=demo \
    PG_AUTH_DATABASE_URL="host=/run/postgresql user=postgres dbname=spicedb"
  sidecar cerbos CERBOS_URL=http://127.0.0.1:3592

  # the relationships already in the tables reach their engines before the snapshot
  for db in openfga spicedb; do
    until [ "$($PSQL -d $db -c 'select count(*) from authz.outbox')" = 0 ]; do sleep 0.3; done
  done
  until [ -S /run/pg_auth/cerbos.sock ]; do sleep 0.3; done

  # train each engine, its sidecar and Postgres on the demo's statements (rolled back)
  for db in openfga spicedb cerbos; do
    psql -X -q -h /run/postgresql -U admin -d $db -v solo=1 -f /etc/demo/train/admin.sql
  done
fi

tmux kill-server 2>/dev/null
CONNECT="psql -h /run/postgresql -d openfga"
exec tmux new-session -s demo -n demo "$CONNECT -U admin -v train=/etc/demo/train/admin.sql; sh" \; \
  select-pane -T " admin · superuser " \; \
  split-window -v -l 8 "sh /usr/local/bin/demo-log" \; \
  select-pane -T " sidecar log " \; \
  select-pane -t 0 \; \
  split-window -v -p 42 "$CONNECT -U anne -v train=/etc/demo/train/anne.sql; sh" \; \
  select-pane -T " anne " \; \
  split-window -h "$CONNECT -U bob -v train=/etc/demo/train/bob.sql; sh" \; \
  select-pane -T " bob " \; \
  select-pane -t 0
