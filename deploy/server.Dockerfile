FROM node:26-bookworm-slim AS web
WORKDIR /build
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts
COPY web/ web/
RUN npm run build

FROM rust:1.99-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY migrations/ migrations/
COPY web/ web/
COPY --from=web /build/web/app.js web/app.js
COPY --from=web /build/web/navigation.js web/navigation.js
COPY --from=web /build/web/workspace.js web/workspace.js
COPY --from=web /build/web/dispatch.js web/dispatch.js
COPY --from=web /build/web/pipeline-connections.js web/pipeline-connections.js
COPY --from=web /build/web/project-graph.js web/project-graph.js
COPY --from=web /build/web/run-chain.js web/run-chain.js
COPY --from=web /build/web/pipeline-review.js web/pipeline-review.js
COPY --from=web /build/web/pipeline-editor.js web/pipeline-editor.js
COPY --from=web /build/web/editor-diagnostics.js web/editor-diagnostics.js
COPY --from=web /build/web/run-graph.js web/run-graph.js
COPY --from=web /build/web/agent-status.js web/agent-status.js
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/build/target \
    cargo build --locked --release -p p4forge-server -p p4forge-agent \
    && mkdir /binaries && cp target/release/p4forge target/release/p4forge-agent /binaries/

FROM debian:bookworm-slim AS perforce
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY scripts/fetch-perforce.sh /fetch-perforce.sh
RUN sh /fetch-perforce.sh /perforce

FROM build AS test
COPY --from=perforce /perforce/p4 /usr/local/bin/p4
COPY examples/ examples/
RUN cargo test --locked --workspace --no-run
ENV P4FORGE_TEST_AGENT_BIN=/binaries/p4forge-agent
CMD ["cargo", "test", "--locked", "--offline", "--workspace", "--", "--include-ignored"]

FROM debian:bookworm-slim AS server
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* && useradd --create-home --uid 10002 p4forge \
    && mkdir -p /var/lib/p4forge/artifacts && chown 10002:10002 /var/lib/p4forge/artifacts
COPY --from=build /binaries/p4forge /usr/local/bin/p4forge
COPY --from=perforce /perforce/p4 /usr/local/bin/p4
USER p4forge
ENV P4FORGE_BIND=0.0.0.0:8088
EXPOSE 8088
HEALTHCHECK --interval=10s --timeout=3s --retries=6 CMD curl --fail --silent http://127.0.0.1:8088/readyz || exit 1
ENTRYPOINT ["p4forge", "serve"]

FROM server AS agent
COPY --from=build /binaries/p4forge-agent /usr/local/bin/p4forge-agent
USER root
HEALTHCHECK NONE
ENTRYPOINT ["p4forge-agent"]
