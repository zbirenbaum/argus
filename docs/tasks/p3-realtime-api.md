# P3: Real-Time Streaming API

**Status**: not started

**Spec reference**: `docs/spec/10-api-reference.md` (WebSocket, SSE)

## Dependencies
- **Blocked by**: P3-query-api (extends same axum server), P1-events (event channel)
- **Blocks**: P5-websocket-approvals (extends WebSocket infrastructure)

## Parallelizable with
- P3-restore, P3-merkle-tree

## What needs to be done
- Extend `crates/argus/src/api/`:

### SSE
- `GET /stdio/{pid}?follow=true` — Server-Sent Events stream of new stdio data
- `GET /events?follow=true` — SSE stream of new events matching filters

### WebSocket
- `ws://…/ws/events` — real-time event stream with filter subscription
- `ws://…/ws/stdio/{pid}` — real-time stdio for a process
- Subscription message: `{ "subscribe": { "path": "...", "pid": N, "type": "..." } }`
- Broadcast from event channel to all matching WebSocket clients

### Infrastructure
- Event broadcast: tokio broadcast channel from event writer to API server
- Client management: track connected clients, clean up on disconnect
- Backpressure: drop events if client falls behind (with gap notification)

## How to test
```bash
cargo test -p argus --lib api -- --ignored
```
Integration tests: connect WebSocket, emit events, verify received in real time. SSE follow mode for stdio.

## Branch
- **Branch**: `p3-realtime-api`
- **Target**: `main`
