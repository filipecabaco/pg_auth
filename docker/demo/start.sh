#!/bin/sh
# Postgres, the three engines and a sidecar for each, then tmux: anne and bob side by side, each a
# psql session as that user. The snapshot is taken at bob's prompt, so visitors restore a machine
# that is already up. When tmux exits the console runs this again; everything is still up.
PSQL="psql -h /run/postgresql -U postgres -X -q -tA"

# Go's runtime, sized for a browser tab: one thread each, collected early
export GOMAXPROCS=1 GOGC=50 GOMEMLIMIT=96MiB

wait_for() { until curl -sf -o /dev/null "$@"; do sleep 0.3; done; }

# a sidecar per engine, as the postgres user (its socket is created 0660)
sidecar() {
  name=$1; shift
  su-exec postgres env "$@" PG_AUTH_SOCKET=/run/pg_auth/$name.sock pg-auth-sidecar > /run/pg_auth/$name-sidecar.log 2>&1 &
}

# how many panes have finished training (train/*.sql)
trained() { $PSQL -d openfga -c "select count(*) from pg_stat_activity where application_name = 'trained'"; }

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
    psql -X -q -h /run/postgresql -U admin -d $db -f /etc/demo/train/engine.sql
  done
fi

# anne's pane trains first and bob's after it, so bob's prompt (the snapshot's ready text) is the
# last thing drawn
tmux kill-server 2>/dev/null
CONNECT="psql -h /run/postgresql -d openfga"
tmux new-session -d -s demo -n demo "$CONNECT -U anne -v train=/etc/demo/train/anne.sql; sh"
tmux select-pane -t demo:0.0 -T " anne "
until [ "$(trained)" -ge 1 ] 2>/dev/null; do sleep 0.2; done
tmux split-window -h -t demo:0.0 "$CONNECT -U bob -v train=/etc/demo/train/bob.sql; sh"
tmux select-pane -t demo:0.1 -T " bob "
exec tmux attach -t demo
