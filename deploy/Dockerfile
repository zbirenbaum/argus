# Argus supervisor — drop-in container instrumentation layer.
#
# Usage as a base layer (zero-code instrumentation):
#   FROM your-app-image
#   COPY --from=argus-supervisor /usr/local/bin/supervisor /usr/local/bin/supervisor
#   COPY --from=argus-supervisor /usr/local/bin/mitmdump /usr/local/bin/mitmdump
#   ENTRYPOINT ["supervisor", "--"]
#   CMD ["your-original-command"]
#
# Usage standalone:
#   docker run --cap-add SYS_PTRACE --security-opt seccomp=unconfined \
#     argus-supervisor -- your-command

FROM rust:1.94-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY scripts/ scripts/

RUN cargo build --release --bin supervisor

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    python3-minimal \
    python3-pip \
    pipx \
    && rm -rf /var/lib/apt/lists/*

# mitmproxy for TLS interception (supervisor spawns mitmdump automatically)
RUN pipx install mitmproxy
ENV PATH="/root/.local/bin:${PATH}"

COPY --from=builder /build/target/release/supervisor /usr/local/bin/supervisor

# Supervisor runs as PID 1, traces the CMD, exits when agent exits.
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/supervisor", "--"]
