# Control API With MySQL

`xaccel-control-api` is the first production-shaped backend service. It replaces
manual token minting and `backend-mock` for the client `connect-intent` path.

## Responsibilities

- Read online nodes from MySQL.
- Read game route rules from MySQL.
- Select a node for `user_id`, `device_id`, and `game_id`.
- Sign a short-lived `xat.v1` token with the selected node secret.
- Store the issued connect intent for audit and later billing.
- Return node address, route target, and credential to the client.

## Database

Create the database and load schema:

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
- game route for `game_id = 8888`;
- route target `127.0.0.1:7777`.

## Run

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

Use `candidates[0].credential.token` in the UDP `probe` packet. The node will
bind `candidates[0].route.target_addr` to the returned session.

## Current Limits

- User entitlement is not checked yet.
- Scheduling picks one online node by route priority and bandwidth quality.
- Node secrets are stored as plaintext for MVP testing. Production should
  encrypt them or fetch them from a secret manager.
- Billing and traffic aggregation are still pending.
