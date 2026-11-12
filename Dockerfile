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
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=web /web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p taskcraft && cp target/release/taskcraft /taskcraft

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
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
