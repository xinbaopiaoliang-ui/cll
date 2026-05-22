# 节点配置与接口草案

本文定义后台与 Linux 节点内核之间的配置和接口。字段来自“服务器管理相关字段”，并扩展运行时必需字段。

## 节点配置 Schema

```json
{
  "schema_version": 1,
  "config_revision": 10001,
  "issued_at": 1779250000,
  "expires_at": 1779250300,
  "node": {
    "id": 1,
    "server_ip": "1.2.3.4",
    "server_port": 666,
    "relay_server_ip": null,
    "relay_server_port": null,
    "is_support_ipv6": false,
    "bandwidth_quality": "normal",
    "disable_quic": false,
    "area": "HK",
    "is_local_ip": false,
    "telecom_ip": null,
    "mobile_ip": null,
    "unicom_ip": null,
    "tag": "free",
    "status": 1
  },
  "transports": {
    "quic_udp": {
      "enabled": true,
      "listen": ["1.2.3.4:666"],
      "max_idle_timeout_sec": 120,
      "keepalive_interval_sec": 15
    },
    "tcp_tls": {
      "enabled": true,
      "listen": ["1.2.3.4:666"]
    },
    "wireguard": {
      "enabled": false
    }
  },
  "limits": {
    "max_sessions": 200000,
    "max_sessions_per_user": 256,
    "max_udp_mappings": 500000,
    "default_user_speed_mbps": 0
  },
  "report": {
    "interval_sec": 30,
    "traffic_batch_sec": 60,
    "metrics_interval_sec": 15
  },
  "security": {
    "config_signature": "base64-signature",
    "allow_backend_ips": ["10.0.0.10/32"]
  }
}
```

## QUIC 开关规则

当 `disable_quic = true`：

- `transports.quic_udp.enabled = false`
- 不监听 QUIC UDP 端口。
- 后台调度不应把该节点作为 UDP 游戏的首选。
- 客户端仍可选择 TCP/TLS 或 WireGuard 类型通道。

当 `disable_quic = false`：

- 开启 QUIC UDP。
- 对 `bandwidth_quality = fast` 的节点启用更激进的 keepalive 和拥塞参数。

## 多运营商入口

如果配置了运营商 IP：

```json
{
  "server_ip": "1.2.3.4",
  "telecom_ip": "1.2.3.5",
  "mobile_ip": "1.2.3.6",
  "unicom_ip": "1.2.3.7"
}
```

节点启动时应监听：

```text
1.2.3.4:666
1.2.3.5:666
1.2.3.6:666
1.2.3.7:666
```

后台给客户端返回哪个 IP，由客户端运营商和健康探测决定。节点只负责确保这些 IP 可监听、可上报质量。

## 后台接口

### 节点握手

```http
POST /api/node/v1/handshake
```

请求：

```json
{
  "node_id": 1,
  "node_version": "0.1.0",
  "os": "linux",
  "arch": "x86_64",
  "boot_id": "uuid",
  "timestamp": 1779250000,
  "nonce": "random",
  "signature": "hmac-sha256"
}
```

响应：

```json
{
  "server_time": 1779250001,
  "config_revision": 10001,
  "websocket": {
    "enabled": true,
    "url": "wss://api.example.com/api/node/v1/events"
  },
  "min_node_version": "0.1.0"
}
```

### 拉取配置

```http
GET /api/node/v1/config?node_id=1&revision=10000
```

响应：返回完整节点配置。支持 `ETag` 或 `config_revision`，没有变化返回 304。

### 上报运行状态

```http
POST /api/node/v1/report
```

请求：

```json
{
  "node_id": 1,
  "config_revision": 10001,
  "status": "ready",
  "traffic": [
    {
      "user_id": 1001,
      "device_id": "pc-001",
      "game_id": 8888,
      "up": 123456,
      "down": 654321
    }
  ],
  "sessions": {
    "active": 1200,
    "udp": 1000,
    "tcp": 200
  },
  "quality": {
    "rtt_ms_p50": 35,
    "rtt_ms_p95": 80,
    "jitter_ms_p95": 12,
    "packet_loss_ppm": 2000
  },
  "system": {
    "cpu": 23.5,
    "mem_used": 2147483648,
    "mem_total": 8589934592,
    "load1": 1.2,
    "rx_bps": 120000000,
    "tx_bps": 118000000
  }
}
```

### WebSocket 事件

后台到节点：

```json
{"event":"config.update","data":{"config_revision":10002}}
{"event":"node.drain","data":{"enabled":true}}
{"event":"user.kick","data":{"user_id":1001,"device_id":"pc-001"}}
{"event":"credential.update","data":{"user_id":1001}}
```

节点到后台：

```json
{"event":"pong","data":{"node_id":1}}
{"event":"node.status","data":{"status":"ready","active_sessions":1200}}
{"event":"node.error","data":{"code":"BIND_FAILED","message":"1.2.3.5:666 bind failed"}}
```

## v0.3.0 Control-Plane Report Contract

The node can optionally send a signed report loop. Standalone installs keep it
disabled unless `--enable-control-plane` is passed.

Config:

```toml
[control]
enabled = true
config_revision = 1
request_timeout_sec = 5

[report]
interval_sec = 30
traffic_batch_sec = 60
metrics_interval_sec = 15
```

Request:

```http
POST /api/node/v1/report
X-Node-Id: 1
X-Node-Timestamp: 1779250000
X-Node-Nonce: 1234-1779250000-1
X-Node-Body-Sha256: base64(sha256(body))
X-Node-Signature: base64(hmac_sha256(secret, canonical))
```

Canonical string:

```text
POST
/api/node/v1/report
1779250000
1234-1779250000-1
base64(sha256(body))
```

The JSON body includes `node_id`, `config_revision`, `node_version`, `status`,
`timestamp`, and a full health snapshot.

## v0.4.0 Client Probe Contract

The TCP/UDP listener still accepts the legacy text payload:

```text
ping
```

and returns the old readiness string. New clients should send a JSON probe
packet over TCP or UDP:

```json
{
  "type": "probe",
  "protocol": "xaccel/1",
  "client_nonce": "client-random",
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 8888,
  "transport": "udp",
  "token": "short-lived-token"
}
```

Response:

```json
{
  "type": "probe.ok",
  "protocol": "xaccel/1",
  "node_id": 1,
  "node_version": "0.4.0",
  "server_time": 1779250000,
  "transport": "udp",
  "requested_transport": "udp",
  "client_nonce": "client-random",
  "session": {
    "session_id": "ps-udp-1779250000-1-2-3-4-50000-1",
    "status": "probe_only",
    "ttl_sec": 30,
    "auth_required": true,
    "credential_present": true,
    "user_id": 1001,
    "device_id": "pc-001",
    "game_id": 8888
  },
  "capabilities": [
    "tcp_probe",
    "udp_probe",
    "token_auth_placeholder",
    "session_stats"
  ]
}
```

Invalid structured requests return:

```json
{
  "type": "probe.error",
  "protocol": "xaccel/1",
  "error": {
    "code": "invalid_probe",
    "message": "protocol must be xaccel/1"
  }
}
```

The token is currently accepted as a placeholder only. The next backend stage
must issue short-lived tokens and the node must verify them before creating
real forwarding sessions.

## 客户端连接意图

游戏规则不是直接给节点消费，而是客户端生成连接意图后由后台调度。

```http
POST /api/client/v1/connect-intent
```

请求：

```json
{
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 8888,
  "platform": "pc",
  "client_isp": "telecom",
  "client_ip": "x.x.x.x",
  "bandwidth_quality": "fast"
}
```

响应：

```json
{
  "intent_id": "uuid",
  "ttl_sec": 120,
  "candidates": [
    {
      "node_id": 1,
      "area": "HK",
      "tag": "free",
      "host": "1.2.3.5",
      "port": 666,
      "transports": ["quic_udp", "tcp_tls"],
      "bandwidth_quality": "normal",
      "probe": {
        "udp": true,
        "tcp": true,
        "ping_payload": "base64"
      },
      "credential": {
        "token": "short-lived-token",
        "expires_at": 1779250120
      }
    }
  ]
}
```
