# syntax=docker/dockerfile:1
# Build from the repository root:
#   docker build -f deploy/Dockerfile -t o5s-server .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p o5s-server -p o5s-cli \
 && install -D target/release/o5s-server /out/o5s-server \
 && install -D target/release/o5s /out/o5s

# Distroless: glibc + CA certificates, no shell, runs as uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/ /usr/local/bin/
COPY deploy/server.docker.toml /etc/o5s/server.toml
# Repository (staging, refs, local packs) and audit log.
VOLUME ["/var/lib/o5s"]
EXPOSE 8080
USER nonroot:nonroot
# SIGTERM triggers a graceful drain (server.shutdown_grace_secs).
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/o5s-server"]
CMD ["serve", "--config", "/etc/o5s/server.toml"]
