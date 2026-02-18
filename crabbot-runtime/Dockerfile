# 1) Builder (Rust 1.92.0)
FROM rust:1.92.0-trixi AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Cache deps
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main(){}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Build real project
COPY . .
# Adjust package name if needed
RUN cargo build --release -p crabbot

# 2) Runtime (small Debian w/ apt + useful tools)
FROM debian:trixi-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tini \
    curl \
    git \
    openssh-client \
    jq \
    bash \
    coreutils \
    findutils \
    procps \
    iputils-ping \
    netcat-openbsd \
    dnsutils \
    unzip \
    xz-utils \
    tar \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
RUN curl -fsSL get.make87.com | sh
# If your binary is named differently, change this path
COPY --from=builder /app/target/release/crabbot /usr/local/bin/crabbot

ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["/usr/local/bin/crabbot","runtime","run"]
