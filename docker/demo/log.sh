#!/bin/sh
# The log pane: what each database asks its sidecar, and what the engine answered. It starts once
# the panes have trained (train/*.sql), so their warm-up requests are not shown.
until [ "$(psql -X -tA -h /run/postgresql -U postgres -d openfga -c \
  "select count(*) from pg_stat_activity where application_name = 'trained'")" -ge 3 ] 2>/dev/null; do
  sleep 0.2
done
printf '\033[2mwhat Postgres asks each engine shows here\033[0m\n'
exec tail -n 0 -F /run/pg_auth/sidecar.log 2>/dev/null
