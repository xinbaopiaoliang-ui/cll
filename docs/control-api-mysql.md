# Control API With MySQL

`xaccel-control-api` is the first production-shaped backend service. It replaces
manual token minting and `backend-mock` for the client `connect-intent` path.

## Responsibilities

- Read online nodes from MySQL.
- Read and manage the game catalog from MySQL.
- Read synced game regions from MySQL.
- Read game route rules from MySQL.
- Select a node for `user_id`, `device_id`, `game_id`, and optional
  `region_id`.
- Sign a short-lived `xat.v1` token with the selected node secret.
- Store the issued connect intent for audit and later billing.
- Return node address, route target, and credential to the client.
- Accept token-protected business backend catalog syncs for games, regions, and
  route rules.

## Database

For a new control-plane server, install MySQL 8 and initialize the `xaccel`
database with the bundled installer:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/mysql8-install.sh | sudo bash -s -- \
  --db-password xaccel_password
```

If you have a SQL backup from the old control-plane server, copy it to the new
server first and import it during installation:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/mysql8-install.sh | sudo bash -s -- \
  --db-password xaccel_password \
  --import-sql /tmp/xaccel.sql
```

The installer creates:

- database `xaccel`;
- user `xaccel` at `127.0.0.1`;
- schema from `db/schema.sql` when no backup is imported;
- root credential file at `/root/.xaccel-mysql-root.cnf` when root password
  auth is used.

Manual equivalent:

```bash
mysql -uroot -p -e "CREATE DATABASE xaccel CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
mysql -uroot -p -e "CREATE USER IF NOT EXISTS 'xaccel'@'%' IDENTIFIED BY 'password';"
mysql -uroot -p -e "GRANT ALL PRIVILEGES ON xaccel.* TO 'xaccel'@'%';"
mysql -uroot -p xaccel < db/schema.sql
```

For local testing, copy the node secret from the Linux node:

```bash
sudo sed -n 's/.*"node_secret": "\([^"]*\)".*/\1/p' /var/lib/xaccel-node/bootstrap-response.json | head -n 1
```

Replace `PASTE_NODE_SECRET` in `db/control-api.seed.example.sql`, then load it:

```bash
mysql -uroot -p xaccel < db/control-api.seed.example.sql
```

The seed creates:

- node `1` at `103.201.131.99:666`;
- game catalog entry for `game_id = 8888`;
- default region `region_id = 1` for `game_id = 8888`;
- game route for `game_id = 8888` with a human-readable `game_name`;
- route target `127.0.0.1:7777`.

## Run

Recommended for the same-server MVP: install the release binary as a systemd
service after MySQL is ready.

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/control-api-install.sh | sudo bash -s -- \
  --database-url 'mysql://xaccel:password@127.0.0.1:3306/xaccel' \
  --listen 127.0.0.1:18080
```

This keeps the API bound to localhost while auth is still MVP-level. Put Nginx,
TLS, and client authentication in front before exposing it publicly.

Useful commands:

```bash
systemctl status xaccel-control-api
journalctl -u xaccel-control-api -f
sudo cat /etc/xaccel-control-api/control-api.env
curl http://127.0.0.1:18080/health
```

For source-level development:

```bash
export DATABASE_URL='mysql://xaccel:password@127.0.0.1:3306/xaccel'

cargo run --manifest-path control-api/Cargo.toml -- \
  --listen 127.0.0.1:18080
```

Health:

```bash
curl http://127.0.0.1:18080/health
```

Connect intent:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

Region-aware connect intent:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"region_id":1,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

Use `candidates[0].credential.token` in the UDP `probe` packet. The node will
bind `candidates[0].route.target_addr` to the returned session.

## Business Backend Sync

The business management backend should own products, users, entitlements, game
metadata, regions, and rule authoring. The control plane keeps an execution copy
for scheduling nodes. Configure a separate sync token:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/control-api-install.sh | sudo bash -s -- \
  --database-url 'mysql://xaccel:password@127.0.0.1:3306/xaccel' \
  --listen 0.0.0.0:18080 \
  --public-base-url http://CONTROL_PUBLIC_IP:18080 \
  --business-sync-token 'change-this-business-sync-token'
```

The same token can be viewed and changed in `/admin` under 系统设置 -> 业务后台对接
Token. Plaintext and edits are only available to super administrators. Saving
from the panel updates `/etc/xaccel-control-api/control-api.env` and applies the
new token immediately.

Then sync catalog data from the business backend:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/business/v1/sync-catalog \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{
    "source":"business-admin",
    "revision":"2026-06-01T10:00:00Z",
    "games":[{
      "game_id":8888,
      "name":"Local Echo Test",
      "platform":"pc",
      "category":"test",
      "categories":["test","local"],
      "status":"enabled",
      "regions":[{
        "region_id":1,
        "name":"Default Region",
        "area":"UNKNOWN",
        "status":"enabled",
        "routes":[{
          "external_id":"route-8888-default",
          "node_id":1,
          "target_addr":"127.0.0.1:7777",
          "protocol":"udp",
          "priority":10,
          "status":"enabled"
        }]
      }]
    }]
  }'
```

`categories`, nested `regions`, and nested `routes` are the recommended shape.
The older top-level `regions` and `route_rules` arrays still work for stepped
syncs. `external_id` should be provided by the business backend when possible.
If it is omitted, the control API derives a stable id from the route fields so
repeated catalog syncs update the same execution route instead of creating
duplicates.

The business backend can also verify the protected integration surface:

```bash
curl -fsSL http://127.0.0.1:18080/api/business/v1/status \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}"
```

After the business backend has checked login, entitlement, device limits, and
game access, it can request the node candidates and short-lived node credential
for the client:

```bash
curl -fsSL -X POST http://127.0.0.1:18080/api/business/v1/connect-intent \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"request_id":"req-1","entitlement_id":"vip-1001","user_id":1001,"device_id":"pc-001","game_id":8888,"region_id":1,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast","client_version":"0.1.0"}'
```

Detailed integration contract: [业务后台对接控制面](business-backend-integration.md).

## Current Limits

- User entitlement is still owned by the business backend and is not checked by
  the control plane yet.
- Scheduling picks one online node by route priority, bandwidth quality, latest
  report freshness, and active session counts. It does not yet use geographic
  latency probes or ISP-specific historical quality.
- Node secrets are stored as plaintext for MVP testing. Production should
  encrypt them or fetch them from a secret manager.
- Billing and traffic aggregation are still pending.
