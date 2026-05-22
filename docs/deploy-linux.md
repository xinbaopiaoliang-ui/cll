# Linux Deployment

This document describes how to deploy the current Linux node.

Current version: `v0.7.0`.

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
- forward authenticated UDP `session.data` packets to a target UDP endpoint;
- optionally report signed health snapshots to the backend control plane.

It does not yet bind forwarding to production game rules or backend
connect-intents.

## 1. Create A Release

From the local repository:

```bash
git tag v0.7.0
git push origin v0.7.0
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
  "node_version": "0.7.0",
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
  "node_version": "0.7.0",
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

Authenticated sessions can also test real UDP target forwarding. Start a tiny
UDP echo target on the node server in another shell:

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

Send `session.data` with a target. Use the `session_id` from a token-authenticated
UDP probe, because target forwarding requires `credential_valid = true`.

```bash
printf '{"type":"session.data","protocol":"xaccel/1","session_id":"PASTE_SESSION_ID","client_nonce":"d2","payload":"aGVsbG8=","target_host":"127.0.0.1","target_port":7777,"response_timeout_ms":200}\n' | nc -u -w 2 YOUR_SERVER_IP 666
```

Expected response shape:

```json
{
  "type": "session.data.ok",
  "node_version": "0.7.0",
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

This is still a relay MVP. The next production step is to bind target forwarding
to backend-issued connect-intents and game rules.

## 8. Optional Control Plane Report

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

## 9. Placeholder Mode

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

## 10. Uninstall

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
  provided, and can forward authenticated packets to a target UDP endpoint.
- Token auth verifies `xat.v1` HMAC tokens when provided, but missing tokens
  are still allowed for standalone testing.
- Control-plane reporting is implemented, but backend config sync and websocket
  commands are still pending.
- Production game-rule binding, relay-node chaining, and token authentication
  enforcement for every client path are still pending.
