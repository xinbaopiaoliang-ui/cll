# xaccel-control-api

Production-shaped Rust control-plane API for XAccel.

This service owns client `connect-intent` scheduling. It reads MySQL node and
route tables through SQLx, selects an online node, signs a short-lived `xat.v1`
credential, stores the intent, and returns the node candidate to the client.
It uses route priority, bandwidth quality, recent node report freshness, and
active session counts when selecting between candidate nodes. It also receives
HMAC-signed node runtime reports and stores them in MySQL.
Admin node management APIs are protected by an admin bearer token. The embedded
dashboard is available at `/admin` and uses the same bearer token in the
browser. The UI is organized as a management console with login, sidebar menus,
overview, node management, game route management, and operations workspaces.
Operators can create nodes, change status, generate bootstrap install commands,
edit config, and manage route rules without direct MySQL edits.
Business-backend catalog sync is separated from admin operations through
`XACCEL_BUSINESS_SYNC_TOKEN`; it upserts games, game regions, and route rules
into the control plane execution copy.

## Run

Prepare MySQL with `db/schema.sql` and seed a test node with
`db/control-api.seed.example.sql`.

```bash
export DATABASE_URL='mysql://xaccel:password@127.0.0.1:3306/xaccel'
export XACCEL_ADMIN_TOKEN='change-this-token'
export XACCEL_BUSINESS_SYNC_TOKEN='change-this-business-sync-token'

cargo run --manifest-path control-api/Cargo.toml -- \
  --listen 127.0.0.1:18080
```

## Endpoints

```text
GET  /health
GET  /admin
POST /api/client/v1/connect-intent
POST /api/business/v1/sync-catalog
POST /api/node/v1/report
POST /api/admin/v1/nodes
GET  /api/admin/v1/nodes
GET  /api/admin/v1/nodes/{node_id}
PATCH /api/admin/v1/nodes/{node_id}/status
POST /api/admin/v1/nodes/{node_id}/bootstrap-token
GET  /api/admin/v1/game-route-rules
POST /api/admin/v1/game-route-rules
PATCH /api/admin/v1/game-route-rules/{rule_id}
DELETE /api/admin/v1/game-route-rules/{rule_id}
POST /api/node/v1/bootstrap
```

Request:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

`region_id` is optional. When present, scheduling prefers matching
game-region routes and falls back to global routes for the same game:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"region_id":1,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

The response candidate includes a `scheduler` object with `route_priority`,
latest report age, report freshness, and active session counters. Operators can
use that block to explain why a node was selected during diagnostics.

`POST /api/node/v1/report` is called by `xaccel-node` when `[control].enabled`
is true. The request uses `X-Node-Id`, `X-Node-Timestamp`, `X-Node-Nonce`,
`X-Node-Body-Sha256`, and `X-Node-Signature` headers.

Admin requests use:

```text
http://CONTROL_PUBLIC_IP:18080/admin
```

The page stores the bearer token in browser local storage and calls the same
admin APIs listed below.

```bash
curl -fsSL http://127.0.0.1:18080/api/admin/v1/nodes \
  -H "Authorization: Bearer ${XACCEL_ADMIN_TOKEN}"
```

Create a node record:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/admin/v1/nodes \
  -H "Authorization: Bearer ${XACCEL_ADMIN_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"name":"node-2","server_ip":"203.0.113.10","server_port":666,"area":"UNKNOWN","bandwidth_quality":"normal"}'
```

Generate a bootstrap install command:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/admin/v1/nodes/1/bootstrap-token \
  -H "Authorization: Bearer ${XACCEL_ADMIN_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"public_base_url":"http://CONTROL_PUBLIC_IP:18080"}'
```

Create a game route rule:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/admin/v1/game-route-rules \
  -H "Authorization: Bearer ${XACCEL_ADMIN_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"game_id":8888,"game_name":"Local Echo Test","node_id":1,"target_addr":"127.0.0.1:7777","protocol":"udp","priority":100,"status":"enabled"}'
```

Sync catalog data from the business backend:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/business/v1/sync-catalog \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"source":"business-admin","games":[{"game_id":8888,"name":"Local Echo Test","platform":"pc","status":"enabled"}],"regions":[{"game_id":8888,"region_id":1,"name":"Default Region","area":"UNKNOWN","status":"enabled"}],"route_rules":[{"external_id":"route-8888-default","game_id":8888,"game_name":"Local Echo Test","region_id":1,"region_name":"Default Region","node_id":1,"target_addr":"127.0.0.1:7777","protocol":"udp","priority":10,"status":"enabled"}]}'
```

`external_id` is recommended for route rules. If it is omitted, the control API
generates a stable id from game, region, node, target, and protocol so repeated
syncs remain idempotent.
