# Build context: sidecar/query.   docker build -t pg_auth-sidecar sidecar/query
# the Rust version mise.toml pins
FROM rust:1.99.1-bookworm AS builder
# coverage builds pass -C instrument-coverage (scripts/coverage.exs)
ARG RUSTFLAGS=""
ENV RUSTFLAGS=${RUSTFLAGS}
WORKDIR /src
# dependencies first, so source edits reuse the compiled dependency layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM debian:bookworm-slim
# reqwest's rustls verifier loads the system CA store when the client is built
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/pg-auth-sidecar /usr/local/bin/pg-auth-sidecar
# The socket is created 0660, so run as the postgres image's uid/gid (999) to share it with
# Postgres. A named volume mounted here starts with this directory's ownership.
RUN mkdir -p /run/pg_auth && chown 999:999 /run/pg_auth
USER 999:999
ENV PG_AUTH_SOCKET=/run/pg_auth/pg_auth.sock
ENTRYPOINT ["pg-auth-sidecar"]
