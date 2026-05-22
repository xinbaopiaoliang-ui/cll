# Linux Deployment

This document describes how to deploy the current Linux node.

Current version: `v0.9.0`.

The node can:

- install through the GitHub-hosted one-click script;
- download the latest GitHub Release artifact;
- verify sha256;
- run as a systemd service;
- expose `127.0.0.1:9876/health`;
- listen on the configured TCP/UDP `server_ip:server_port`;
- count basic TCP/UDP traffic;
- answer structured `xaccel/1` client probe requests with a short-lived
  probe session id;
- verify optional `xat.v1` HMAC client tokens;
- keep short-lived UDP probe sessions and answer `session.data` echo packets;
- bind connect-intent target routes from signed client tokens;
- forward authenticated UDP `session.data` packets to the bound UDP endpoint;
- issue development connect-intent responses through `backend-mock`;
- optionally report signed health snapshots to the backend control plane.

It does not yet fetch production game rules or connect-intents from a real
backend API.

## 1. Create A Release

From the local repository:

```bash
git tag v0.9.0
git push origin v0.9.0
```

GitHub Actions will publish:

```text
xaccel-node-linux-x86_64.tar.gz
xaccel-node-linux-x86_64.tar.gz.sha256
```

Wait until the `Release xaccel-node` workflow succeeds.

## 2. Install On Linux

Replace `YOUR_SERVER_IP` with the Linux server public IP:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666
```

Optional firewall opening:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666 \
  --open-firewall
```

## 3. Check Service

```bash
systemctl status xaccel-node
journalctl -u xaccel-node -f
```

Check files:

```bash
cat /etc/xaccel-node/config.toml
sudo cat /var/lib/xaccel-node/bootstrap-response.json
sudo cat /var/lib/xaccel-node/identity.json
```

Health:

```bash
curl http://127.0.0.1:9876/health
```

## 4. Check TCP/UDP Listener

```bash
ss -lntup | grep ':666'
```

If `nc` is installed:

```bash
printf 'ping\n' | nc -w 2 YOUR_SERVER_IP 666
printf 'ping\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Call health again:

```bash
curl http://127.0.0.1:9876/health
```

Expected fields:

```json
{
  "listeners": {
    "udp_listening": true,
    "tcp_listening": true,
    "listen_addr": "YOUR_SERVER_IP:666"
  },
  "traffic": {
    "udp_rx_packets": 1,
    "tcp_accepted": 1
  }
}
```

## 5. Check Structured Client Probe

The legacy `ping` probe is kept for quick manual checks. Clients should use a
JSON probe request so the node can allocate a probe session and return its
runtime capabilities.

TCP:

```bash
printf '{"type":"probe","protocol":"xaccel/1","client_nonce":"n1","user_id":1001,"device_id":"pc-001","game_id":8888,"transport":"tcp"}\n' | nc -w 2 YOUR_SERVER_IP 666
```

UDP:

```bash
printf '{"type":"probe","protocol":"xaccel/1","client_nonce":"n2","user_id":1001,"device_id":"pc-001","game_id":8888,"transport":"udp"}\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Expected response shape:

```json
{
  "type": "probe.ok",
  "protocol": "xaccel/1",
  "node_id": 1,
  "node_version": "0.9.0",
  "transport": "udp",
  "requested_transport": "udp",
  "session": {
    "session_id": "ps-udp-...",
    "status": "probe_only",
    "ttl_sec": 30,
    "auth_required": true,
    "credential_present": false,
    "credential_valid": false,
    "credential_expires_at": null,
    "user_id": 1001,
    "device_id": "pc-001",
    "game_id": 8888
  }
}
```

Call health again and check:

```json
{
  "sessions": {
    "probe_sessions_total": 2,
    "probe_rejected": 0,
    "auth_missing": 2,
    "auth_ok": 0,
    "auth_failed": 0
  }
}
```

## 6. Check Token Auth

During standalone testing, the node can generate a short-lived client token
from its local identity. Production clients should receive this token from the
backend, not from the node shell.

```bash
TOKEN=$(/usr/local/bin/xaccel-node --config /etc/xaccel-node/config.toml \
  --make-client-token \
  --token-user-id 1001 \
  --token-device-id pc-001 \
  --token-game-id 8888 \
  --token-ttl-sec 120)
```

Use the token in a probe:

```bash
printf '{"type":"probe","protocol":"xaccel/1","client_nonce":"n3","user_id":1001,"device_id":"pc-001","game_id":8888,"transport":"udp","token":"'"$TOKEN"'"}\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Expected token fields in the response:

```json
{
  "session": {
    "credential_present": true,
    "credential_valid": true,
    "credential_expires_at": 1779250120
  }
}
```

Invalid tokens return `probe.error` and increment `sessions.auth_failed`.

## 7. Check UDP Session Data

After a successful UDP probe, copy the returned `session.session_id` and use it
quickly. Probe sessions currently expire after 30 seconds.

Send a small base64 payload. `aGVsbG8=` is `hello`.

```bash
printf '{"type":"session.data","protocol":"xaccel/1","session_id":"PASTE_SESSION_ID","client_nonce":"d1","payload":"aGVsbG8="}\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Expected response shape:

```json
{
  "type": "session.data.ok",
  "protocol": "xaccel/1",
  "node_version": "0.9.0",
  "transport": "udp",
  "session_id": "ps-udp-...",
  "status": "echo",
  "payload": "aGVsbG8=",
  "payload_bytes": 5
}
```

Call health again and check:

```json
{
  "sessions": {
    "active_udp_sessions": 1,
    "udp_session_rx_packets": 1,
    "udp_session_rx_bytes": 5,
    "udp_session_tx_packets": 1,
    "udp_session_miss": 0,
    "udp_session_expired": 0
  }
}
```

Without a target endpoint this remains an echo integration check.

Authenticated sessions can also test real UDP target forwarding. In `v0.9.0`,
the preferred path is to put the target route into the signed token, which
models a backend-issued connect-intent. Start a tiny UDP echo target on the node
server in another shell:

```bash
python3 - <<'PY'
import socket

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 7777))
print("udp echo target listening on 127.0.0.1:7777")
while True:
    data, addr = s.recvfrom(2048)
    s.sendto(b"upstream:" + data, addr)
PY
```

Generate a token that includes a connect-intent id and target route:

```bash
TOKEN=$(/usr/local/bin/xaccel-node --config /etc/xaccel-node/config.toml \
  --make-client-token \
  --token-user-id 1001 \
  --token-device-id pc-001 \
  --token-game-id 8888 \
  --token-ttl-sec 120 \
  --token-intent-id intent-local-udp-7777 \
  --token-target-addr 127.0.0.1:7777)
```

Use that token in a UDP probe and copy the returned `session_id`:

```bash
printf '{"type":"probe","protocol":"xaccel/1","client_nonce":"n4","user_id":1001,"device_id":"pc-001","game_id":8888,"transport":"udp","token":"'"$TOKEN"'"}\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Then send `session.data` without any target fields. The node uses the route
bound to the session during probe:

```bash
printf '{"type":"session.data","protocol":"xaccel/1","session_id":"PASTE_SESSION_ID","client_nonce":"d2","payload":"aGVsbG8=","response_timeout_ms":200}\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Expected response shape:

```json
{
  "type": "session.data.ok",
  "node_version": "0.9.0",
  "status": "forwarded",
  "payload": "dXBzdHJlYW06aGVsbG8=",
  "payload_bytes": 14,
  "request_payload_bytes": 5,
  "target": {
    "address": "127.0.0.1:7777"
  },
  "relay": {
    "mode": "udp_target",
    "timed_out": false,
    "upstream_tx_bytes": 5,
    "upstream_rx_bytes": 14
  }
}
```

For development only, `session.data` may still carry `target_addr` or
`target_host` + `target_port` when the token does not include a route. A
token-bound route takes priority over client-provided target fields.

Health should include relay counters:

```json
{
  "sessions": {
    "udp_relay_tx_packets": 1,
    "udp_relay_tx_bytes": 5,
    "udp_relay_rx_packets": 1,
    "udp_relay_rx_bytes": 14,
    "udp_relay_timeout": 0,
    "udp_relay_error": 0
  }
}
```

This is still a relay MVP. The next production step is to replace the mock
backend with production scheduling, storage, and game rules.

## 8. Check Backend Connect-Intent Mock

`v0.9.0` adds a small development backend that signs the same `xat.v1` token the
node verifies. Run it from a repository checkout on a machine with Rust
installed. Use the same node secret that standalone install wrote on the Linux
server:

```bash
NODE_SECRET=$(sudo sed -n 's/.*"node_secret": "\([^"]*\)".*/\1/p' /var/lib/xaccel-node/bootstrap-response.json | head -n 1)
```

Start the mock backend:

```bash
XACCEL_NODE_SECRET="$NODE_SECRET" cargo run --manifest-path backend-mock/Cargo.toml -- \
  --listen 127.0.0.1:18080 \
  --node-id 1 \
  --node-host YOUR_SERVER_IP \
  --node-port 666 \
  --target-addr 127.0.0.1:7777
```

Request a client connect-intent:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

The response includes `candidates[0].host`, `candidates[0].port`,
`candidates[0].route.target_addr`, and `candidates[0].credential.token`. Use
that token in the UDP `probe` packet. The node will bind the target route to the
returned `session_id`, so later `session.data` packets no longer need target
fields.

## 9. Optional Control Plane Report

Standalone installs keep backend reporting disabled by default because
`https://api.example.com` is only a placeholder. When the real backend endpoint
exists, enable signed node reports during install:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://YOUR_BACKEND_DOMAIN \
  --server-ip YOUR_SERVER_IP \
  --server-port 666 \
  --enable-control-plane
```

The node posts to:

```text
POST /api/node/v1/report
```

with HMAC headers:

```text
X-Node-Id
X-Node-Timestamp
X-Node-Nonce
X-Node-Body-Sha256
X-Node-Signature
```

Health exposes report status under `control_plane`.

## 10. Placeholder Mode

Only use this when the GitHub Release is not ready and you want to test the
installer/systemd path:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666 \
  --allow-placeholder
```

## 11. Uninstall

Keep data and logs:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/uninstall.sh | sudo bash
```

Purge everything:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/uninstall.sh | sudo bash -s -- --purge
```

## Current Limits

- GitHub Actions currently builds Linux `x86_64` only.
- TCP/UDP listener currently returns legacy and structured probe responses.
- UDP `session.data` verifies the session id, echoes payload when no target is
  bound, and forwards authenticated packets to the route target from the signed
  token.
- Token auth verifies `xat.v1` HMAC tokens when provided, but missing tokens
  are still allowed for standalone testing.
- Control-plane reporting is implemented, but backend config sync and websocket
  commands are still pending.
- Production backend storage/scheduling, game-rule lookup, relay-node chaining,
  and token authentication enforcement for every client path are still pending.
