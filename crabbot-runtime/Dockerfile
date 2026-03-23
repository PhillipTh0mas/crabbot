# 1) Builder (Rust 1.92.0)
FROM rust:1.92.0-trixie AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# -- Cache deps ---------------------------------------------------------------
# Copy workspace-level manifests and lock file
COPY Cargo.toml Cargo.lock ./
COPY crabbot-runtime/Cargo.toml crabbot-runtime/Cargo.toml
COPY crabbot-shared/Cargo.toml  crabbot-shared/Cargo.toml
COPY crabbot-ui/Cargo.toml      crabbot-ui/Cargo.toml

# Create dummy sources so cargo can resolve the workspace and fetch/compile deps
RUN mkdir -p crabbot-runtime/src && echo "fn main(){}" > crabbot-runtime/src/main.rs && \
    mkdir -p crabbot-shared/src  && echo "" > crabbot-shared/src/lib.rs && \
    mkdir -p crabbot-ui/src      && echo "" > crabbot-ui/src/lib.rs && \
    cargo build --release -p crabbot && \
    rm -rf crabbot-runtime/src crabbot-shared/src

# -- Build real project --------------------------------------------------------
COPY crabbot-shared/ crabbot-shared/
COPY crabbot-runtime/ crabbot-runtime/

RUN cargo build --release -p crabbot

# 2) Runtime (small Debian w/ apt + useful tools)
FROM debian:trixie-slim

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
COPY --from=builder /app/target/release/crabbot /usr/local/bin/crabbot

ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["/usr/local/bin/crabbot","runtime","run"]
