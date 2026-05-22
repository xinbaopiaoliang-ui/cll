# xaccel-control-api

Production-shaped Rust control-plane API for XAccel.

This service owns client `connect-intent` scheduling. It reads MySQL node and
route tables through SQLx, selects an online node, signs a short-lived `xat.v1`
credential, stores the intent, and returns the node candidate to the client.

## Run

Prepare MySQL with `db/schema.sql` and seed a test node with
`db/control-api.seed.example.sql`.

```bash
export DATABASE_URL='mysql://xaccel:password@127.0.0.1:3306/xaccel'

cargo run --manifest-path control-api/Cargo.toml -- \
  --listen 127.0.0.1:18080
```

## Endpoints

```text
GET  /health
POST /api/client/v1/connect-intent
```

Request:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```
