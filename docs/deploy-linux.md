# Linux Deployment

This document describes how to deploy the current Linux node.

Current version: `v0.2.0`.

The node can:

- install through the GitHub-hosted one-click script;
- download the latest GitHub Release artifact;
- verify sha256;
- run as a systemd service;
- expose `127.0.0.1:9876/health`;
- listen on the configured TCP/UDP `server_ip:server_port`;
- count basic TCP/UDP traffic.

It does not yet implement real game traffic forwarding.

## 1. Create A Release

From the local repository:

```bash
git tag v0.2.0
git push origin v0.2.0
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

## 5. Placeholder Mode

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

## 6. Uninstall

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
- TCP/UDP listener currently returns probe responses and records counters.
- Real game acceleration, relay, user authentication, and control-plane sync
  are still pending.

