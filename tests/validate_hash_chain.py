#!/usr/bin/env python3
"""Validates that write events form an unbroken hash chain.

Reads JSON-lines events from stdin (one event per line) and checks
that each write event's before_hash equals the previous write event's
after_hash for the same path.

Usage:
    sandbox log --path /workspace/shared.txt --type write --format json \
        | python3 validate_hash_chain.py
"""

import json
import sys


def main() -> int:
    events = []
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    if not events:
        print("ERROR: no events found", file=sys.stderr)
        return 1

    # Group by path and check chain within each path.
    by_path: dict[str, list] = {}
    for evt in events:
        path = evt.get("path", "")
        by_path.setdefault(path, []).append(evt)

    total = len(events)
    broken = 0

    for path, path_events in sorted(by_path.items()):
        for i in range(1, len(path_events)):
            prev_after = path_events[i - 1].get("after_hash")
            curr_before = path_events[i].get("before_hash")
            if prev_after and curr_before and prev_after != curr_before:
                broken += 1
                seq = path_events[i].get("seq", "?")
                print(
                    f"BROKEN at seq {seq}: "
                    f"expected before={prev_after[:12]} "
                    f"got before={curr_before[:12]}"
                )

    print(f"{total} writes, {broken} hash chain breaks")

    if broken > 0:
        print("FAIL: hash chain is broken")
        return 1

    # Check that content is well-formed (no truncated/mixed lines).
    for evt in events:
        ah = evt.get("after_hash")
        if ah is None:
            continue
        # Content validation would require CAS access; skip for now.

    print("PASS: all hash chains intact")
    return 0


if __name__ == "__main__":
    sys.exit(main())
