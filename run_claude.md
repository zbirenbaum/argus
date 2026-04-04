docker run -it --rm --name argus-test \
    --cap-add SYS_PTRACE \
    --security-opt seccomp=unconfined \
    --security-opt apparmor=unconfined \
    -v ~/.claude:/home/agent/.claude \
    -v ~/.claude.json:/home/agent/.claude.json \
    -e HOME=/home/agent \
    -e RUST_LOG=off \
    -p 9090:9090 -p 8000:8000 \
    argus-claude claude --dangerously-skip-permissions
