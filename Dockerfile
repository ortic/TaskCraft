# syntax=docker/dockerfile:1.7
# ---- frontend ----
FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---- backend (embeds the web app) ----
FROM rust:1.96-bookworm AS server
# SAML single sign-on (`--build-arg FEATURES=saml`) links libxml2 / libxmlsec1 for XML signatures
ARG FEATURES=""
RUN if [ -n "$FEATURES" ]; then apt-get update && apt-get install -y --no-install-recommends libxml2-dev libxmlsec1-dev libclang-dev clang pkg-config && rm -rf /var/lib/apt/lists/*; fi
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=web /web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p taskcraft --features "$FEATURES" && cp target/release/taskcraft /taskcraft

# ---- runtime ----
FROM debian:bookworm-slim
ARG FEATURES=""
RUN extra=""; case "$FEATURES" in *saml*) extra="libxml2 libxmlsec1 libxmlsec1-openssl" ;; esac; \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates $extra && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data taskcraft && mkdir -p /data && chown -R taskcraft /data
COPY --from=server /taskcraft /usr/local/bin/taskcraft
COPY LICENSE-MIT LICENSE-APACHE /usr/share/doc/taskcraft/
USER taskcraft
ENV TASKCRAFT_BIND=0.0.0.0:8788 \
    TASKCRAFT_DATA_DIR=/data
EXPOSE 8788
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s CMD ["taskcraft", "healthcheck"]
ENTRYPOINT ["taskcraft"]
CMD ["serve"]
