#!/usr/bin/env bash
# Run a traced Claude agent with argus-api.
#
# Terminal 1: ./scripts/run-demo.sh "Build a todo app"
# Terminal 2: ./scripts/watch-events.sh
set -euo pipefail

PROMPT="${1:-Build a simple todo web app with Node.js and Express. SQLite storage. HTML frontend. Run on port 3000.}"

docker rm -f argus-demo 2>/dev/null || true

exec docker run --rm -it --name argus-demo \
  --cap-add SYS_PTRACE \
  --security-opt seccomp=unconfined \
  --security-opt apparmor=unconfined \
  -e CLAUDE_CODE_OAUTH_TOKEN="$DEV_CLAUDE_CODE_OAUTH_TOKEN" \
  -e HOME=/home/agent \
  -p 9090:9090 \
  -p 8000:8000 \
  -v "$(cd "$(dirname "$0")/.." && pwd)":/build \
  argus-arm64 \
  bash -c '
    useradd -m -s /bin/bash -u 1000 agent 2>/dev/null || true
    mkdir -p /tmp/data /tmp/workspace
    chown -R agent:agent /tmp/workspace

    cat > /tmp/config.yaml <<YAML
agent_command: ["true"]
workspace_dir: /tmp/workspace
data_dir: /tmp/data
listen_addr: "0.0.0.0:9090"
run_as:
  uid: 1000
tls:
  upstream_insecure: true
tree:
  batch_size: 1
YAML

    /build/target/aarch64-unknown-linux-musl/debug/argus-api \
      --supervisor 127.0.0.1:9090 --listen 0.0.0.0:8000 --db /tmp/events.db \
      --event-log-dir /tmp/data/events \
      2>/tmp/api.log &

    echo "============================================" >&2
    echo "  Supervisor:  http://localhost:9090"         >&2
    echo "  Query API:   http://localhost:8000"         >&2
    echo "  WebSocket:   ws://localhost:9090/ws"        >&2
    echo "============================================" >&2

    exec /build/target/aarch64-unknown-linux-musl/debug/supervisor \
      --agent-id demo --config /tmp/config.yaml \
      -- claude -p --dangerously-skip-permissions --model haiku "'"$PROMPT"'"
  '
