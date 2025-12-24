# hyper-proxy

A small HAProxy-style reverse proxy built on `web-service` with REST management for HTTP(S) and WS(S)
backends. Incoming traffic is accepted over HTTP/1.1, HTTP/2, and HTTP/3.

## Features
- HTTP/1.1 + HTTP/2 (TLS ALPN) and HTTP/3 (QUIC) front door
- WebSocket proxying for `ws://` and `wss://` upstreams
- REST API for adding/removing backends and toggling load-balancing mode
- Load balancing: `queue` (round-robin) and `leastconn`

## Quick start
1. Create a `.env` (see `.env.example`) with base64-encoded TLS cert/key.
2. Run:

```bash
cd hyper-proxy
cargo run
```

## REST API
All endpoints live under `/api`.

- `GET /api/health` -> `{ "status": "ok" }`
- `GET /api/backends` -> list http + ws backends with active counts
- `POST /api/backends` -> add a backend
- `DELETE /api/backends/{id}` -> remove a backend
- `GET /api/balancer` -> get current mode
- `PUT /api/balancer` -> set mode

### Add backend example
```bash
curl -k https://localhost:443/api/backends \
  -H 'content-type: application/json' \
  -d '{"url":"https://origin.example.com","max_connections":100}'
```

### Switch load-balancing mode
```bash
curl -k https://localhost:443/api/balancer \
  -X PUT \
  -H 'content-type: application/json' \
  -d '{"mode":"leastconn"}'
```

## Notes
- HTTP proxying uses a low-level `hyper` client and forwards headers (minus hop-by-hop headers).
- WebSocket proxying is bidirectional and stays open until either side closes.
- `queue` is round-robin with optional per-backend `max_connections` caps and will wait for a slot.
- Logs are JSON structured with per-request spans and `x-request-id` propagation.
- Enable debug logs with `RUST_LOG=hyper_proxy=debug`.
# hyper-proxy
