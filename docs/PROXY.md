# MITM Proxy & TLS Capture

How argus captures HTTP traffic and TLS key material from traced agents.

## Proxy modes

Three modes control how agent traffic reaches mitmdump:

```yaml
tls:
  proxy_mode: env          # env (default) | transparent | off
```

| Mode | How traffic is routed | mitmdump mode | Env vars set |
|-|-|-|-|
| `env` | `HTTPS_PROXY`/`HTTP_PROXY` env vars | regular (HTTP CONNECT) | proxy + keylog + certs |
| `transparent` | `connect()` sockaddr rewritten via ptrace | `--mode transparent` (SNI) | keylog + certs |
| `off` | No routing (direct connections) | not started | keylog only |

**`env`** (default) covers ~95% of agent traffic. Most HTTP libraries honor `HTTPS_PROXY`. Fails for statically linked binaries and programs that ignore proxy env vars.

**`transparent`** rewrites the destination address in `connect()` at the syscall level before the kernel executes it. Works on statically linked binaries, Go programs, anything that calls `connect()`. Mitmdump reads the SNI from the TLS ClientHello to determine the upstream. Limited to TLS ports (443, 8443).

**`off`** disables all proxy routing. `SSLKEYLOGFILE` still captures TLS key material for passive decryption. No HTTP flow events.

## Architecture (env mode)

```
Agent process (traced)
  |
  |  HTTPS_PROXY=http://127.0.0.1:8080
  |  SSLKEYLOGFILE=/data/tls/keylog.txt
  |  SSL_CERT_FILE=/data/tls/mitmproxy-ca-cert.pem
  |
  v
mitmdump (port 8080, regular mode)
  |  - TLS1 Agent<->mitmdump (agent trusts argus CA)
  |  - TLS2 mitmdump<->upstream (mitmdump verifies upstream cert)
  |  - argus_addon.py writes NDJSON to flows.jsonl
  |
  v
tls-watcher thread (polls every 200ms)
  - KeylogWatcher reads SSLKEYLOGFILE -> TlsKeys events
  - FlowWatcher reads flows.jsonl -> HttpRequest/HttpResponse events
       |
       v
  event channel (mpsc) -> event writer -> stdout JSONL
```

## Architecture (transparent mode)

```
Agent process (traced)
  |
  |  connect(fd, {1.2.3.4:443}) --[ptrace]--> connect(fd, {127.0.0.1:8080})
  |  SSLKEYLOGFILE=/data/tls/keylog.txt
  |  SSL_CERT_FILE=/data/tls/mitmproxy-ca-cert.pem
  |
  v
mitmdump (port 8080, --mode transparent)
  |  Reads SNI from TLS ClientHello to determine upstream
  |  Same TLS1/TLS2 split as env mode
  |
  v
tls-watcher thread (same as env mode)
```

The supervisor intercepts `connect()` via seccomp-ptrace. On entry:
1. Reads the `sockaddr` from tracee memory
2. If TCP port 443/8443 and not loopback, saves the original destination
3. Writes `{127.0.0.1:8080}` into the tracee's `sockaddr` via `process_vm_writev`
4. Resumes the syscall -- the kernel connects to mitmdump instead
5. The event records the *original* destination (not the rewritten one)

## Two TLS sessions per request

When the agent makes an HTTPS request through the proxy, two separate TLS sessions are established:

**TLS1: Agent -> mitmdump.** In env mode, the agent connects via HTTP CONNECT, then negotiates TLS inside the tunnel. In transparent mode, the agent sends a raw TLS ClientHello directly. Either way, the agent sees mitmdump's dynamically-generated cert (signed by the argus CA). The agent trusts this because we inject `SSL_CERT_FILE` pointing at `mitmproxy-ca-cert.pem`.

**TLS2: mitmdump -> upstream.** mitmdump opens its own TLS connection to the real server. This is where upstream cert verification matters -- mitmdump needs to trust whatever cert the upstream presents.

Both sessions write key material to `SSLKEYLOGFILE`. A single curl request produces ~10-15 `tls_keys` events (5 TLS 1.3 secrets per session x 2+ sessions).

## Upstream certificate verification

mitmdump must verify TLS② — the connection to the real upstream server. Three modes:

| Config | mitmdump flag | Use case |
|-|-|-|
| *(default)* | *(none)* | Public internet; system trust store handles it |
| `upstream_ca: /path/ca.pem` | `ssl_verify_upstream_trusted_ca=<path>` | Internal services with private CA / corporate PKI |
| `upstream_insecure: true` | `ssl_insecure=true` | Dev/test with self-signed certs |

```yaml
# Production: internal services signed by corporate PKI
tls:
  upstream_ca: /etc/argus/internal-ca.pem

# Dev/test: self-signed certs
tls:
  upstream_insecure: true
```

Without the right mode, mitmdump rejects the upstream cert with a 502. The addon never fires, `flows.jsonl` stays empty, and no `http_request`/`http_response` events are emitted. `tls_keys` events still appear because `SSLKEYLOGFILE` is written by the agent's TLS library regardless of the proxy.

## Addon script embedding

The Python addon (`scripts/argus_addon.py`) is embedded in the supervisor binary via `include_str!` and written to `data_dir/argus_addon.py` at startup. No external file dependency.

The addon hooks mitmdump's `response()` callback. For each completed HTTP flow it writes a single JSON line to stdout (redirected to `flows.jsonl` by the supervisor):

```json
{"request":{"method":"GET","url":"https://example.com/","headers":[["Host","example.com"]]},"response":{"status_code":200,"headers":[["Content-Type","text/html"]],"body":"PGh0bWw+..."}}
```

Bodies are base64-encoded. The FlowWatcher decodes them and stores in CAS; events reference content by hash.

## TLS watcher thread

`crates/supervisor/src/tls_watcher.rs` — a dedicated thread that polls two files:

1. **SSLKEYLOGFILE** via `KeylogWatcher`: reads new lines, stores each in CAS, emits `TlsKeys` events.
2. **flows.jsonl** via `FlowWatcher`: reads new NDJSON lines, decodes base64 bodies into CAS, emits `HttpRequest`/`HttpResponse` events.

The watcher uses its own `SequenceGenerator` starting at 1,000,000 to avoid collisions with the tracer's sequence space. Both generators are lock-free (`AtomicU64`). Events from both sources merge into the same `mpsc` channel and appear interleaved in the JSONL output, ordered by timestamp.

**Shutdown ordering:** The tls-watcher stops first (drains any remaining data), then mitmdump is killed. This ensures the final poll captures anything written between the last interval and shutdown.

## CONNECT tunnel gotcha (env mode)

In env mode, mitmdump proxies HTTPS via HTTP CONNECT tunneling. The proxy first receives a `CONNECT host:port` request, establishes the tunnel, then the client negotiates TLS inside it. This means:

- An upstream server that calls `handle_request()` once will serve the CONNECT and exit before the actual GET arrives. Test servers must handle at least two requests.
- The CONNECT itself does not appear in the addon output -- only the decrypted HTTP flows inside the tunnel do.

In transparent mode, there is no CONNECT tunnel. The client sends a raw TLS ClientHello directly, so test servers only need to handle one request (the actual GET).

## Event examples

```json
{"seq":1000000,"type":"tls_keys","pid":0,"fd":-1,"keylog_line_hash":"c689..."}
{"seq":1000025,"type":"http_request","pid":0,"method":"GET","url":"https://localhost:8443/","headers_hash":"0faf..."}
{"seq":1000026,"type":"http_response","pid":0,"status":200,"headers_hash":"a5c0...","body_hash":"805e..."}
```

`pid: 0` and `fd: -1` on TLS watcher events because the watcher doesn't know which process/fd generated the traffic — it reads from files, not from ptrace. Correlation is done by timestamp and by matching `tls_keys` client_random values to `connect` events from the tracer.

## Testing

All commands run inside the `argus-x86` container (x86_64 with ptrace/seccomp support).

```bash
# Build the supervisor
docker exec argus-x86 cargo build --target x86_64-unknown-linux-musl -p supervisor

# Run validation test 8 (TLS capture)
docker exec argus-x86 ./tests/validate.sh 8

# Run all unit tests
docker exec argus-x86 cargo test -p argus -p supervisor

# Manual end-to-end test with full event output
docker exec argus-x86 bash -c '
mkdir -p /tmp/argus-test-data /tmp/argus-test-workspace

# Ephemeral self-signed cert + HTTPS server
openssl req -x509 -newkey rsa:2048 \
    -keyout /tmp/k.pem -out /tmp/c.pem \
    -days 1 -nodes -subj "/CN=localhost" 2>/dev/null
python3 -c "
import ssl, http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        b = json.dumps({\"ok\":1}).encode()
        self.send_response(200)
        self.send_header(\"Content-Length\", str(len(b)))
        self.end_headers()
        self.wfile.write(b)
    def log_message(self, *a): pass
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(\"/tmp/c.pem\", \"/tmp/k.pem\")
s = http.server.HTTPServer((\"127.0.0.1\", 8443), H)
s.socket = ctx.wrap_socket(s.socket, server_side=True)
s.handle_request()  # CONNECT tunnel
s.handle_request()  # actual GET
" &
sleep 0.5

# Config with upstream_insecure for self-signed cert
cat > /tmp/tls-test.yaml <<EOF
workspace_dir: /tmp/argus-test-workspace
data_dir: /tmp/argus-test-data
tls:
  upstream_insecure: true
EOF

# Run curl through the supervisor, sleep so tls-watcher can drain
target/x86_64-unknown-linux-musl/debug/supervisor \
    --agent-id demo --config /tmp/tls-test.yaml \
    -- bash -c "curl -sk https://localhost:8443/; sleep 1" \
    2>/dev/null | jq .

rm -f /tmp/k.pem /tmp/c.pem /tmp/tls-test.yaml
'
```

Expected output includes `agent_start`, `exec`, `tls_keys` (10-15+), `http_request`, `http_response`, and `exit` events.
