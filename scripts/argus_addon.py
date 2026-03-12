"""Mitmdump addon that emits each HTTP flow as a single-line JSON object.

Output goes to stdout (captured by the supervisor via pipe). Each line
is a complete JSON object with request and optional response fields.
Bodies are base64-encoded so binary payloads survive serialization.

Usage:
    mitmdump -s argus_addon.py --set flow_detail=0 --quiet
"""

import base64
import json
import sys


def response(flow):
    """Called by mitmdump when a response is received."""
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
        if flow.response.content:
            resp["body"] = base64.b64encode(flow.response.content).decode()
        data["response"] = resp

    print(json.dumps(data, separators=(",", ":")), flush=True)
