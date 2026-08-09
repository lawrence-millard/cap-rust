# syntax=docker/dockerfile:1

# Build stage
FROM rust:1-slim-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r cap -g 10001 \
    && useradd -r -g cap -u 10001 -d /data cap \
    && mkdir -p /data \
    && chown cap:cap /data
COPY --from=builder /app/target/release/cap-server /usr/local/bin/cap-server
USER cap
ENV STORAGE_DIR=/data \
    PORT=8080
EXPOSE 8080
VOLUME ["/data"]
LABEL org.opencontainers.image.source="https://github.com/lawrence-millard/cap-rust" \
      org.opencontainers.image.description="Lightweight CAP-compatible server for Cap Desktop" \
      org.opencontainers.image.licenses="AGPL-3.0"
CMD ["cap-server"]
