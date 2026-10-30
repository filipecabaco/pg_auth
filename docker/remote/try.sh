#!/bin/sh
# The demo's story, with pg_auth asking its sidecar over HTTPS. Run `docker compose up --build -d`
# here first.
set -e
cd "$(dirname "$0")"
q() { docker compose exec -T postgres psql -X -q -tA -U "$1" -d postgres -c "$2"; }
ids() { q "$1" "select coalesce(string_agg(id::text, ', ' order by id), 'nothing') from documents"; }

printf 'waiting for the sidecar to push the rules and the relationships'
until [ "$(q postgres "select count(*) from authz.rules() where pushed_at is null")" = 0 ] &&
      [ "$(q postgres "select count(*) from authz_internal.outbox")" = 0 ]; do printf .; sleep 1; done
echo
q anne "select demo_reset()" >/dev/null
sleep 1

echo "pg_auth asks:   $(q postgres "show pg_auth.sidecar_url")  (and no sidecar shares its socket)"
echo "anne sees:      $(ids anne)"
echo "bob sees:       $(ids bob)"
echo "anne adds bob:  $(q anne "insert into memberships values (1, 'bob', 'member') returning 'done'")"
until [ "$(ids bob)" != nothing ]; do sleep 0.2; done
echo "bob sees:       $(ids bob)"

echo
echo "and someone who reaches the sidecar without the secret:"
docker pull -q curlimages/curl:8.11.1 >/dev/null
code=$(docker run --rm --network pg_auth-remote_default curlimages/curl:8.11.1 -sk -o /dev/null -w '%{http_code}' \
  -X POST https://sidecar:8443/v1/ask -d '{"op":"list","subject":"user:anne","permission":"viewer","type":"document"}')
echo "  an unsigned request: HTTP $code"
