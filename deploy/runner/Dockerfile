# syntax=docker/dockerfile:1.7
# The runner of TaskCraft's AI coding agents, on its own: a container that holds nothing but the
# checkout it works on. Build from the repository's root:
#   docker build -f deploy/runner/Dockerfile -t taskcraft-runner .
# Add your project's toolchain in an image of your own (FROM taskcraft-runner) when the agent
# should build and test (--allow-commands).
FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p taskcraft-runner && cp target/release/taskcraft-runner /taskcraft-runner

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home /work runner
COPY --from=build /taskcraft-runner /usr/local/bin/taskcraft-runner
USER runner
WORKDIR /work
# TASKCRAFT_RUNNER_SERVER and TASKCRAFT_RUNNER_TOKEN (from `taskcraft-runner register`) say whom it works for.
# --once: one run, then the container ends — start it again for a fresh one per run.
ENTRYPOINT ["taskcraft-runner"]
CMD ["run", "--once"]
