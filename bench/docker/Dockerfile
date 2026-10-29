# syntax=docker/dockerfile:1
# Build context: repo root.   docker build -f bench/docker/Dockerfile -t pg_auth-bench .
ARG PG_MAJOR=18

FROM postgres:${PG_MAJOR} AS builder
ARG PG_MAJOR
# the versions mise.toml pins
ARG RUST_VERSION=1.99.1
ARG PGRX_VERSION=0.19.3
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl ca-certificates git pkg-config libssl-dev \
      clang libclang-dev libicu-dev libreadline-dev zlib1g-dev bison flex \
      postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION}
RUN cargo install cargo-pgrx --version ${PGRX_VERSION} --locked
# Use the official image's own pg_config so the build matches the runtime server exactly.
RUN cargo pgrx init --pg${PG_MAJOR}=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config

# coverage builds pass -C instrument-coverage (scripts/coverage.exs)
ARG RUSTFLAGS=""
ENV RUSTFLAGS=${RUSTFLAGS}
WORKDIR /src
COPY Cargo.toml Cargo.lock pg_auth.control ./
COPY .cargo ./.cargo
COPY sql ./sql
COPY src ./src
# cache mounts keep crates and compiled dependencies between builds: a change to src/ rebuilds
# pg_auth only (the package step copies its output out of the cached target/)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo pgrx package --pg-config /usr/lib/postgresql/${PG_MAJOR}/bin/pg_config \
    && mkdir -p /out \
    && cp -r target/release/pg_auth-pg${PG_MAJOR}/. /out/

FROM postgres:${PG_MAJOR}
ARG PG_MAJOR
# pgrx package mirrors the absolute pg_config paths under /out
COPY --from=builder /out/usr/lib/postgresql/${PG_MAJOR}/lib/ /usr/lib/postgresql/${PG_MAJOR}/lib/
COPY --from=builder /out/usr/share/postgresql/${PG_MAJOR}/extension/ /usr/share/postgresql/${PG_MAJOR}/extension/
# Applies to every database cluster initialised from this image.
RUN echo "shared_preload_libraries = 'pg_auth'" >> /usr/share/postgresql/postgresql.conf.sample
