# xaccel-backend-mock

Small development backend for issuing `connect-intent` responses.

It signs `xat.v1` client tokens with the same HMAC format used by
`xaccel-node`, including `intent_id` and `route.target_addr` claims. This lets
the client test the production-shaped flow without a real panel/database yet.

## Run

From a repository checkout, pass the same node secret that the Linux node uses.
For a standalone node, it is stored in
`/var/lib/xaccel-node/bootstrap-response.json`.

```bash
export XACCEL_NODE_SECRET='PASTE_NODE_SECRET'

cargo run --manifest-path backend-mock/Cargo.toml -- \
  --listen 127.0.0.1:18080 \
  --node-id 1 \
  --node-host 103.201.131.99 \
  --node-port 666 \
  --target-addr 127.0.0.1:7777
```

## Request

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

Use `candidates[0].credential.token` in the client's `probe` packet. The node
will bind `candidates[0].route.target_addr` to the returned probe session.
