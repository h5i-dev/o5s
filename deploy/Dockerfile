# syntax=docker/dockerfile:1
# Build from the repository root:
#   docker build -f deploy/Dockerfile -t pico2peta-server .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p pico2peta-server -p pico2peta-cli \
 && install -D target/release/pico2peta-server /out/pico2peta-server \
 && install -D target/release/pico2peta /out/pico2peta

# Distroless: glibc + CA certificates, no shell, runs as uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/ /usr/local/bin/
COPY deploy/server.docker.toml /etc/pico2peta/server.toml
# Repository (staging, refs, local packs) and audit log.
VOLUME ["/var/lib/pico2peta"]
EXPOSE 8080
USER nonroot:nonroot
# SIGTERM triggers a graceful drain (server.shutdown_grace_secs).
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/pico2peta-server"]
CMD ["serve", "--config", "/etc/pico2peta/server.toml"]
