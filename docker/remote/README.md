# pg_auth with a sidecar over HTTPS

Postgres reaches its sidecar over the network instead of a Unix socket, as when the sidecar runs
on another host or on a platform next to the database (`docs/proposals/external-providers.md`).
Three services, Postgres with pg_auth, SpiceDB, and the sidecar, which shares nothing with
Postgres but the network; and a one-off container that makes the test certificates. The tables and rules are the demo's (`docker/demo/sql`,
`docker/demo/rules/spicedb.zed`).

    docker compose up --build -d
    ./try.sh
    docker compose down -v

`try.sh` waits for the sidecar to push the rules and deliver the relationships, then tells the
demo's story: anne sees documents 1 to 3, bob none; anne adds bob to her organisation, and bob
sees them. Last, a request sent to the sidecar without the secret is refused with 401.

What makes it work:

- **Postgres** (`setup.sh`): `pg_auth.sidecar_url = 'https://sidecar:8443'`, the secret in
  `pg_auth.sidecar_secret_file`, and the test CA in `pg_auth.sidecar_ca_file`. The sidecar logs in
  as `sidecar`, a role holding only `authz_sidecar`.
- **The sidecar**: `PG_AUTH_LISTEN`, the same secret in `PG_AUTH_SECRET_FILE`, and its
  certificate in `PG_AUTH_TLS_CERT` and `PG_AUTH_TLS_KEY`. Its own connection to Postgres is
  encrypted too, the server's certificate checked (`sslmode=verify-full`).
- **`certs.sh`** makes the test CA, a certificate for the names `sidecar` and `postgres`, and the secret, into a
  volume both read. For an example only: a deployment gives each side only what it needs, the key
  to the sidecar alone.

`psql -h 127.0.0.1 -p 55432 -U postgres` (password `postgres`) reaches Postgres from the host.
