# The in-browser demo: Postgres with pg_auth, and OpenFGA, SpiceDB and Cerbos each with a sidecar,
# driven from tmux (anne and bob side by side). Built into a static site by snowglobe
# (https://github.com/filipecabaco/snowglobe), which boots this image on an emulated arm64 machine
# in the visitor's tab. `mise run demo` builds it.
#
# The base is docker/alpine (pg_auth:alpine-arm64): the extension and sidecar built for Alpine.
# The engines keep their data in memory, which suits a tab; a deployment gives each a datastore.
ARG BASE=pg_auth:alpine-arm64
FROM openfga/openfga:v1.22.0 AS openfga
FROM authzed/spicedb:v1.56.2 AS spicedb
FROM ghcr.io/cerbos/cerbos:0.56.0 AS cerbos

FROM ${BASE}
RUN apk add --no-cache tmux curl jq su-exec
# the engines are static Go binaries: they run on Alpine as shipped
COPY --from=openfga /openfga /usr/local/bin/openfga
COPY --from=spicedb /usr/local/bin/spicedb /usr/local/bin/spicedb
COPY --from=cerbos /cerbos /usr/local/bin/cerbos

# each engine's rules, in its own language, with pg_auth's notes: stored in Postgres at build and
# pushed to the engines by their sidecars at boot (the page's Setup view shows these files)
COPY rules/ /etc/demo/rules/
COPY cerbos/conf.yaml /etc/demo/cerbos/conf.yaml
COPY sql/ /etc/demo/sql/
COPY train/ /etc/demo/train/

# A cluster made at build time and sized for a browser tab: small shared memory, no durability
# work (the disk is the visitor's memory and goes away with the tab). One database per engine,
# each pointed at its own sidecar.
ENV PGDATA=/var/lib/postgresql/data
RUN install -d -o postgres -g postgres -m 700 "$PGDATA" && \
    su-exec postgres initdb -D "$PGDATA" -U postgres --auth=trust --encoding=UTF8 --no-locale >/dev/null && \
    printf '%s\n' \
      "listen_addresses = ''" \
      "unix_socket_directories = '/run/postgresql'" \
      "max_connections = 20" \
      "shared_buffers = 16MB" \
      "dynamic_shared_memory_type = mmap" \
      "fsync = off" \
      "synchronous_commit = off" \
      "full_page_writes = off" \
      "wal_level = minimal" \
      "max_wal_senders = 0" \
      "autovacuum = off" \
      "jit = off" \
      "shared_preload_libraries = 'pg_auth'" \
      >> "$PGDATA/postgresql.conf" && \
    install -d -o postgres -g postgres /run/postgresql && \
    su-exec postgres pg_ctl -D "$PGDATA" -w -s start && \
    psql="psql -X -q -v ON_ERROR_STOP=1 -h /run/postgresql -U postgres" && \
    $psql -f /etc/demo/sql/roles.sql && \
    for db in openfga spicedb cerbos; do \
      $psql -c "create database $db" && \
      $psql -c "alter database $db set pg_auth.sidecar_socket = '/run/pg_auth/$db.sock'" && \
      case $db in \
        cerbos) $psql -d $db -f /etc/demo/sql/cerbos.sql ;; \
        openfga) $psql -d $db -v engine=openfga -v rules_name=model -v rules_file=/etc/demo/rules/openfga.fga \
                   -f /etc/demo/sql/relationships.sql ;; \
        spicedb) $psql -d $db -v engine=spicedb -v rules_name=schema -v rules_file=/etc/demo/rules/spicedb.zed \
                   -f /etc/demo/sql/relationships.sql ;; \
      esac || exit 1; \
    done && \
    su-exec postgres pg_ctl -D "$PGDATA" -w -s stop

COPY tmux.conf /root/.tmux.conf
COPY psqlrc /root/.psqlrc
COPY start.sh /usr/local/bin/start-demo

ENV LANG=C.UTF-8 HOME=/root TERM=xterm-256color
WORKDIR /root
CMD ["/usr/local/bin/start-demo"]

# Snapshot once both panes are up, at bob's prompt. The warm pattern covers the files the first
# statements read: the databases' catalogs and tables.
LABEL snowglobe.title="pg_auth: authorization enforced by Postgres" \
      snowglobe.ready="bob=>" \
      snowglobe.memory="1024" \
      snowglobe.warm="^/var/lib/postgresql/data/(base|global)/"
