# syntax=docker/dockerfile:1
# LLM Router — Multi-Stage-Build.
# rusqlite "bundled" kompiliert SQLite aus den C-Quellen mit: im Container wird
# nur ein C-Compiler gebraucht, keine System-Installation von SQLite.
# Runtime-Image: glibc (Debian slim), reqwest nutzt rustls -> kein OpenSSL nötig.

FROM rust:1.93-slim AS build
WORKDIR /build

# C-Compiler für libsqlite3-sys (bundled) + Werkzeuge
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc libc6-dev make \
 && rm -rf /var/lib/apt/lists/*

# Dependency-Cache-Layer: erst nur die Manifests, damit cargo die ~250 Crates
# genau einmal kompiliert (bei jedem Code-Change bleibt dieser Layer gecacht).
COPY Cargo.toml ./
COPY crates/router-config/Cargo.toml crates/router-config/
COPY crates/router-core/Cargo.toml crates/router-core/
COPY crates/router-providers/Cargo.toml crates/router-providers/
COPY crates/router-api/Cargo.toml crates/router-api/
COPY crates/router-admin/Cargo.toml crates/router-admin/
RUN mkdir -p crates/router-config/src crates/router-core/src crates/router-providers/src \
             crates/router-api/src crates/router-admin/src \
 && touch crates/router-config/src/lib.rs crates/router-core/src/lib.rs crates/router-providers/src/lib.rs \
        crates/router-api/src/lib.rs crates/router-admin/src/lib.rs \
        crates/router-api/src/main.rs crates/router-admin/src/main.rs \
 && cargo build --release --workspace --bins || true

# echter Quellcode -> finaler Build nutzt den Deps-Cache
COPY . .
RUN cargo build --release --workspace --bins

FROM debian:trixie-slim AS runtime
# ca-certificates: HTTPS gegen OpenRouter/DeepSeek; curl: Docker-HEALTHCHECK
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=build /build/target/release/router /usr/local/bin/router
COPY --from=build /build/target/release/router-admin /usr/local/bin/router-admin

# Config + SQLite-Datenbank kommen per Volume nach /config; non-root (UID 10001)
RUN mkdir -p /config/data && chown -R 10001:10001 /config
USER 10001

ENV ROUTER_CONFIG=/config/router.toml
EXPOSE 4123
ENTRYPOINT ["router"]
