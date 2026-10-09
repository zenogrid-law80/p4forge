FROM debian:bookworm-slim AS download
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY scripts/fetch-perforce.sh /fetch-perforce.sh
# Official 2026.1 binaries, pinned by SHA-256 for both supported architectures.
RUN sh /fetch-perforce.sh /download
FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home perforce \
    && mkdir /data && chown perforce:perforce /data
COPY --from=download /download/p4 /download/p4d /usr/local/bin/
USER perforce
VOLUME /data
EXPOSE 1666
HEALTHCHECK --interval=5s --timeout=3s --retries=20 CMD p4 -p localhost:1666 info -s > /dev/null || exit 1
ENTRYPOINT ["p4d", "-r", "/data", "-p", "1666", "-L", "/data/server.log", "-J", "/data/journal"]
