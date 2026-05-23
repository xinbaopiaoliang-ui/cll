# xaccel-control-api

Production-shaped Rust control-plane API for XAccel.

This service owns client `connect-intent` scheduling. It reads MySQL node and
route tables through SQLx, selects an online node, signs a short-lived `xat.v1`
credential, stores the intent, and returns the node candidate to the client.
It also receives HMAC-signed node runtime reports and stores them in MySQL.
Admin node management APIs are protected by an admin bearer token.

## Run

Prepare MySQL with `db/schema.sql` and seed a test node with
`db/control-api.seed.example.sql`.

```bash
export DATABASE_URL='mysql://xaccel:password@127.0.0.1:3306/xaccel'
export XACCEL_ADMIN_TOKEN='change-this-token'

cargo run --manifest-path control-api/Cargo.toml -- \
  --listen 127.0.0.1:18080
```

## Endpoints

```text
GET  /health
POST /api/client/v1/connect-intent
POST /api/node/v1/report
GET  /api/admin/v1/nodes
GET  /api/admin/v1/nodes/{node_id}
PATCH /api/admin/v1/nodes/{node_id}/status
POST /api/admin/v1/nodes/{node_id}/bootstrap-token
POST /api/node/v1/bootstrap
```

Request:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

`POST /api/node/v1/report` is called by `xaccel-node` when `[control].enabled`
is true. The request uses `X-Node-Id`, `X-Node-Timestamp`, `X-Node-Nonce`,
`X-Node-Body-Sha256`, and `X-Node-Signature` headers.

Admin requests use:

```bash
curl -fsSL http://127.0.0.1:18080/api/admin/v1/nodes \
  -H "Authorization: Bearer ${XACCEL_ADMIN_TOKEN}"
```

Generate a bootstrap install command:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/admin/v1/nodes/1/bootstrap-token \
  -H "Authorization: Bearer ${XACCEL_ADMIN_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"public_base_url":"http://CONTROL_PUBLIC_IP:18080"}'
```
