"""Mitmdump addon that emits each HTTP flow as a single-line JSON object.

Output goes to stdout (captured by the supervisor via pipe). Each line
is a complete JSON object with request and optional response fields.
Bodies are base64-encoded so binary payloads survive serialization.

Streaming responses (SSE, chunked transfer) are forwarded to the agent
in real time while simultaneously accumulating the full body for capture.

Usage:
    mitmdump -s argus_addon.py --set flow_detail=0 --quiet
"""

import base64
import json


def responseheaders(flow):
    """Install a streaming capture filter on every response.

    The filter forwards each chunk to the client unchanged (zero added
    latency) while accumulating a copy for the response() callback.
    """
    chunks = []

    def capture(chunk):
        chunks.append(chunk)
        return chunk

    flow.response.stream = capture
    flow.metadata["_chunks"] = chunks


def response(flow):
    """Called by mitmdump when a response is complete (including streamed)."""
    data = {
        "request": {
            "method": flow.request.method,
            "url": flow.request.url,
            "headers": list(flow.request.headers.items(multi=True)),
        }
    }
    if flow.request.content:
        data["request"]["body"] = base64.b64encode(flow.request.content).decode()

    if flow.response:
        resp = {
            "status_code": flow.response.status_code,
            "headers": list(flow.response.headers.items(multi=True)),
        }

        # Prefer captured stream chunks; fall back to buffered content.
        chunks = flow.metadata.get("_chunks")
        if chunks:
            body = b"".join(chunks)
        else:
            body = flow.response.content

        if body:
            resp["body"] = base64.b64encode(body).decode()

        data["response"] = resp

    print(json.dumps(data, separators=(",", ":")), flush=True)
