# FAQ

## TLS / HTTP Capture

### I see `tls_keys` events but no `http_request`/`http_response`

Three things must all be true for HTTP flow capture:

1. **mitmdump must be installed.** The supervisor logs a warning and continues without it. Check for `mitmdump unavailable` in stderr.

2. **The agent's traffic must route through the proxy.** In `env` mode (default), the supervisor sets `HTTPS_PROXY=http://127.0.0.1:8080` on the agent process, but some tools ignore it:
   - curl respects `HTTPS_PROXY` but may skip it for hosts in `NO_PROXY` or `no_proxy`.
   - Some HTTP libraries have their own proxy settings or hardcoded bypasses.
   - If the agent overrides or clears `HTTPS_PROXY`, traffic goes direct.
   - Statically linked binaries and Go programs may not honor proxy env vars.

   If env mode isn't capturing traffic, try `proxy_mode: transparent` which rewrites `connect()` at the syscall level:
   ```yaml
   tls:
     proxy_mode: transparent
   ```
   This works on any program that calls `connect()`, regardless of whether it honors proxy env vars. Limited to TLS ports (443, 8443).

3. **mitmdump must be able to connect to the upstream server.** By default mitmdump verifies the upstream TLS cert against the system trust store. If the upstream uses a self-signed cert or private CA, mitmdump rejects it with a 502 and the addon never fires. Fix this with config:

   ```yaml
   # For internal services with a private CA:
   tls:
     upstream_ca: /etc/argus/internal-ca.pem

   # For dev/test with self-signed certs (never use in production):
   tls:
     upstream_insecure: true
   ```

### I see `tls_keys` events but the count seems high

Each TLS 1.3 session produces 5 key log lines (CLIENT_HANDSHAKE_TRAFFIC_SECRET, SERVER_HANDSHAKE_TRAFFIC_SECRET, CLIENT_TRAFFIC_SECRET_0, SERVER_TRAFFIC_SECRET_0, EXPORTER_SECRET). When traffic goes through the proxy there are multiple TLS sessions — client-to-proxy and proxy-to-upstream — so a single curl request can produce 10-15+ `tls_keys` events. This is expected.

### The local HTTPS test server only serves one request then the test fails

mitmdump uses HTTP CONNECT tunneling for HTTPS proxying. The CONNECT handshake consumes one "request" from the server's perspective before the actual GET arrives. If the test server calls `handle_request()` only once, it serves the CONNECT and exits before the real request. Use `handle_request()` twice (or `serve_forever()` with a timeout).

### `http_request`/`http_response` events are missing the body

The body is stored in CAS by content hash. The event contains `body_hash` — use the `/cas/{hash}` API endpoint or inspect the local CAS directory to retrieve the actual content. Bodies from the mitmdump addon arrive base64-encoded and are decoded before CAS storage.

### How do I verify upstream certs securely in production?

Generate a CA for your internal services and set `upstream_ca` in the supervisor config:

```yaml
tls:
  upstream_ca: /etc/argus/internal-ca.pem
```

mitmdump will verify upstream certs against this CA instead of the system trust store. This covers the common case where agents call internal APIs signed by corporate PKI or a service mesh CA.

Do **not** use `upstream_insecure: true` in production. It disables all upstream certificate verification, which means mitmdump will happily connect to any server presenting any certificate — defeating the purpose of TLS for the upstream leg.
