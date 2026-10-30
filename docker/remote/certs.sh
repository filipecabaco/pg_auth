#!/bin/sh
# A test CA, a certificate it signs for the names `sidecar` and `postgres`, and the secret Postgres and the sidecar
# sign with, made once into the `tls` volume. Readable by both containers' users: an example's
# shortcut, where a deployment keeps the key to the sidecar's user and the secret to each side's.
set -e
cd /tls
[ -f secret ] && exit 0
apk add --no-cache openssl >/dev/null
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 -subj "/CN=pg_auth example CA" \
  -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign -keyout ca.key -out ca.pem 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=sidecar -keyout server.key -out server.csr 2>/dev/null
printf 'subjectAltName=DNS:sidecar,DNS:postgres\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' > ext.cnf
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 -extfile ext.cnf -out server.pem 2>/dev/null
openssl rand -hex 32 > secret
chmod 0644 ca.pem server.pem server.key secret
# Postgres's copy of the key: its own (uid 999 in the official image), unreadable to others
cp server.key postgres.key && chown 999:999 postgres.key && chmod 0600 postgres.key
rm -f ca.key ca.srl server.csr ext.cnf
