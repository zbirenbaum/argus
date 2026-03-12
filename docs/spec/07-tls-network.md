# TLS & Network Capture

SSLKEYLOGFILE and proxy env vars are set before exec'ing the agent (step 6 of startup, see `01-supervisor.md`). If these aren't set before the agent starts, TLS sessions can't be recovered retroactively.

## Layer 1: SSLKEYLOGFILE

Set `SSLKEYLOGFILE=/data/tls/keylog.txt` in agent environment. TLS libraries write per-session keys as handshakes complete.

**Covers:** OpenSSL, BoringSSL, NSS, GnuTLS, wolfSSL — Python, Node.js, curl, wget, most C/C++.
**Gaps:** Go crypto/tls (ignores env var), Rust rustls (opt-in).

Supervisor watches keylog file, stores entries in CAS, emits `tls_keys` events. Raw socket bytes captured via ptrace. Keys + encrypted bytes enable offline decryption.

## Layer 2: MITM Proxy

In-container mitmdump on 127.0.0.1:8080. Custom CA cert injected into trust stores.

**Environment (set before exec):**
```
HTTPS_PROXY=http://127.0.0.1:8080
HTTP_PROXY=http://127.0.0.1:8080
SSL_CERT_FILE=/etc/ssl/certs/argus-ca.pem
NODE_EXTRA_CA_CERTS=/etc/ssl/certs/argus-ca.pem
REQUESTS_CA_BUNDLE=/etc/ssl/certs/argus-ca.pem
```

**Covers:** Python requests/urllib3, Node.js http/https, curl, wget, most HTTP libraries.
**Gaps:** Non-HTTP TLS, tools ignoring proxy vars, Go custom transports.

Proxy captures structured request/response (method, URL, headers, body, status). Emits `http_request` / `http_response` events with body stored in CAS.

## Layered Deduplication

Both layers overlap. Layer 2 preferred for HTTP (structured). Layer 1 as fallback for non-HTTP TLS. Deduplicate by fd + timestamp + content.

## Network Capture Tiers

| Tier | What | When |
|------|------|------|
| 1 | socket/connect/accept metadata | Always |
| 2 | Byte counts per connection, TLS SNI | Always |
| 3 | Full payload on localhost connections | Always (agent-to-tool traffic) |
| 3 | Full payload on plaintext connections | Always |
| 3 | Raw encrypted bytes + keylog for TLS | Always (decrypt offline) |

## Container Image Requirements

The argus-base image (see `09-multi-agent.md`) must include:
- mitmdump binary
- CA certificate at `/etc/ssl/certs/argus-ca.pem`
- CA private key generated at first run, persisted to `/data/tls/ca-key.pem`

## Database Protocol Capture

Recommended: use uninstrumented external database (PostgreSQL, Redis) as cluster service. Queries captured via network interception:

| Database | Protocol | Parsing |
|----------|----------|---------|
| PostgreSQL | Frontend/backend wire protocol | Structured |
| Redis | RESP (text-based) | Trivial |
| MySQL | Client/server protocol | Structured |
| gRPC | HTTP/2 + protobuf (via TLS decryption) | Post-processing |

For embedded SQLite: file-level capture is opaque (page writes, not SQL). Recommend wrapping via HTTP API or using network-accessible alternative. File group locking (.db + -wal + -shm) for atomic snapshots when direct file access is unavoidable.

## Configuration

```yaml
tls:
  sslkeylogfile: /data/tls/keylog.txt
  proxy:
    enabled: true
    listen: 127.0.0.1:8080
    ca_cert: /etc/ssl/certs/argus-ca.pem
    ca_key: /data/tls/ca-key.pem

network:
  capture_localhost: true
  capture_metadata: true
  capture_payloads_plaintext: true
```
