# Linux Deployment

This document describes how to deploy the current Linux node.

Current version: `v0.4.0`.

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
- optionally report signed health snapshots to the backend control plane.

It does not yet implement real game traffic forwarding.

## 1. Create A Release

From the local repository:

```bash
git tag v0.4.0
git push origin v0.4.0
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
  "node_version": "0.4.0",
  "transport": "udp",
  "requested_transport": "udp",
  "session": {
    "session_id": "ps-udp-...",
    "status": "probe_only",
    "ttl_sec": 30,
    "auth_required": true,
    "credential_present": false,
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
    "probe_rejected": 0
  }
}
```

## 6. Optional Control Plane Report

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

## 7. Placeholder Mode

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

## 8. Uninstall

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
- Control-plane reporting is implemented, but backend config sync and websocket
  commands are still pending.
- Real game acceleration, relay forwarding, and token authentication enforcement
  are still pending.
