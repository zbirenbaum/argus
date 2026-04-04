# Argus supervisor + API — traced Claude Code environment.
#
# Build:
#   docker build -t argus-claude -f deploy/Dockerfile .
#
# Run:
#   docker run -it --rm --name argus-claude \
#     --cap-add SYS_PTRACE \
#     --security-opt seccomp=unconfined \
#     --security-opt apparmor=unconfined \
#     -v ~/.claude:/home/agent/.claude \
#     -v ~/.claude.json:/home/agent/.claude.json \
#     -p 9090:9090 -p 8000:8000 \
#     argus-claude

FROM rust:1.94-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY scripts/ scripts/

RUN cargo build --release --bin supervisor --bin argus-api

# ── runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    python3-minimal \
    python3-pip \
    pipx \
    && rm -rf /var/lib/apt/lists/*

# Node.js (Claude Code requires >= 18)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Claude Code CLI
RUN npm install -g @anthropic-ai/claude-code

# mitmproxy for TLS interception
RUN pipx install mitmproxy
ENV PATH="/root/.local/bin:${PATH}"

# Agent user (supervisor drops to uid 1000 via run_as)
RUN useradd -m -s /bin/bash -u 1000 agent

COPY --from=builder /build/target/release/supervisor /usr/local/bin/supervisor
COPY --from=builder /build/target/release/argus-api /usr/local/bin/argus-api

# Supervisor config: full proxy capture, unsafe certs, API on 9090
RUN cat > /etc/argus.yaml <<'EOF'
workspace_dir: /workspace
data_dir: /data
listen_addr: "0.0.0.0:9090"
run_as:
  uid: 1000
tls:
  proxy_mode: env
  upstream_insecure: true
tree:
  batch_size: 1
capture:
  content:
    paths:
      - "/workspace/**"
EOF

RUN mkdir -p /data /workspace && chown -R agent:agent /workspace

VOLUME ["/data", "/workspace"]
WORKDIR /workspace

EXPOSE 9090 8000

COPY <<'ENTRY' /entrypoint.sh
#!/bin/bash
set -eu

# Start argus-api in background (connects to supervisor WS on :9090)
argus-api \
  --supervisor 127.0.0.1:9090 \
  --listen 0.0.0.0:8000 \
  --db /data/events.db \
  --event-log-dir /data/events \
  2>/data/api.log &
API_PID=$!
trap "kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null" EXIT

exec supervisor --config /etc/argus.yaml -- "$@"
ENTRY
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
CMD ["claude", "--dangerously-skip-permissions"]
