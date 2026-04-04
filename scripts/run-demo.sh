#!/usr/bin/env bash
# Run a traced Claude agent with argus-api inside tmux.
#
# ./scripts/run-demo.sh "Build a todo app"
# docker exec -it argus-demo tmux attach -t claude
set -euo pipefail

docker rm -f argus-demo 2>/dev/null || true

exec docker run -it --rm --name argus-demo \
  --cap-add SYS_PTRACE \
  --security-opt seccomp=unconfined \
  --security-opt apparmor=unconfined \
  -e CLAUDE_CODE_OAUTH_TOKEN="$DEV_CLAUDE_CODE_OAUTH_TOKEN" \
  -e HOME=/home/agent \
  -p 9090:9090 \
  -p 8000:8000 \
  -v "$(cd "$(dirname "$0")/.." && pwd)":/build \
  -v "$HOME/.claude:/home/agent/.claude" \
  -v "$HOME/.claude.json:/home/agent/.claude.json" \
  argus-arm64 \
  bash -c '
    apt-get update -qq && apt-get install -y -qq tmux >/dev/null 2>&1
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
    API_PID=$!
    trap "kill $API_PID 2>/dev/null; wait $API_PID 2>/dev/null" EXIT

    echo "============================================" >&2
    echo "  Supervisor:  http://localhost:9090"         >&2
    echo "  Query API:   http://localhost:8000"         >&2
    echo "  Dashboard:   http://localhost:8000"          >&2
    echo "============================================" >&2

    RUST_LOG=off \
    /build/target/aarch64-unknown-linux-musl/debug/supervisor \
      --agent-id demo --config /tmp/config.yaml \
      -- claude --dangerously-skip-permissions
  '
