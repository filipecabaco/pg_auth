# syntax=docker/dockerfile:1
# pg_auth and its sidecar on Alpine (musl), for the in-browser demo: snowglobe boots Alpine arm64
# images only. Build context: repo root.
#
#   docker build --platform linux/arm64 -f docker/alpine/Dockerfile -t pg_auth:alpine-arm64 .
ARG PG_MAJOR=18

FROM postgres:${PG_MAJOR}-alpine AS builder
ARG PG_MAJOR
# the versions mise.toml pins
ARG RUST_VERSION=1.99.1
ARG PGRX_VERSION=0.19.3
# gcc links (rustup's musl toolchain calls `cc`); clang/llvm for pgrx's bindgen; cmake and perl for
# aws-lc (the sidecar's TLS); icu-dev for the server headers
RUN apk add --no-cache rustup gcc g++ clang clang-dev llvm-dev musl-dev openssl-dev icu-dev cmake perl make git linux-headers
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH \
    # an extension is a shared library: musl's default static C runtime cannot build one
    RUSTFLAGS="-C target-feature=-crt-static"
RUN rustup-init -y --profile minimal --default-toolchain ${RUST_VERSION}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install cargo-pgrx --version ${PGRX_VERSION} --locked
# the base image's own pg_config, so the build matches the server it runs on
RUN cargo pgrx init --pg${PG_MAJOR}=/usr/local/bin/pg_config

WORKDIR /src
COPY Cargo.toml Cargo.lock pg_auth.control ./
COPY .cargo ./.cargo
COPY sql ./sql
COPY src ./src
COPY sidecar/query ./sidecar/query
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/src/sidecar/query/target \
    cargo pgrx package --pg-config /usr/local/bin/pg_config \
    && mkdir -p /out && cp -r target/release/pg_auth-pg${PG_MAJOR}/. /out/ \
    && cd sidecar/query && cargo build --release --locked \
    && install -D target/release/pg-auth-sidecar /out/usr/local/bin/pg-auth-sidecar

FROM postgres:${PG_MAJOR}-alpine
# pgrx package mirrors pg_config's absolute paths (/usr/local/lib/postgresql, /usr/local/share/...)
COPY --from=builder /out/usr/ /usr/
