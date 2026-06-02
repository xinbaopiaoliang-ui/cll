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
  "node_version": "0.31.3",
  "os": "linux",
  "arch": "x86_64",
  "boot_id": "uuid",
  "timestamp": 1779250000,
  "nonce": "random",
  "config_revision": 1,
  "listen_addr": "0.0.0.0:666"
}
```

响应：

```json
{
  "status": "ok",
  "node_id": 1,
  "server_time": 1779250001,
  "config_revision": 10001,
  "websocket": {
    "enabled": false,
    "url": null
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
config_poll_interval_sec = 30

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
  "node_version": "0.12.0",
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
    "credential_valid": true,
    "credential_expires_at": 1779250120,
    "user_id": 1001,
    "device_id": "pc-001",
    "game_id": 8888
  },
  "capabilities": [
    "tcp_probe",
    "udp_probe",
    "token_auth_hmac_v1",
    "udp_session_echo",
    "udp_target_relay",
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

## v0.5.0 Client Token Contract

The client token format is:

```text
xat.v1.base64url(payload_json).base64url(hmac_sha256(secret, "xat.v1.base64url(payload_json)"))
```

Payload:

```json
{
  "node_id": 1,
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 8888,
  "intent_id": "intent-local-udp-7777",
  "route": {
    "target_addr": "127.0.0.1:7777",
    "protocol": "udp"
  },
  "expires_at": 1779250120,
  "issued_at": 1779250000,
  "nonce": "random"
}
```

The node verifies:

- HMAC signature with `node_secret`.
- `node_id` matches this node.
- `expires_at` is still in the future.
- If `route` is present, `route.protocol` must be `udp` and
  `route.target_addr` must be non-empty.
- Optional request fields `user_id`, `device_id`, and `game_id` match token claims.

During standalone development, the node can mint a short-lived test token:

```bash
/usr/local/bin/xaccel-node --config /etc/xaccel-node/config.toml \
  --make-client-token \
  --token-user-id 1001 \
  --token-device-id pc-001 \
  --token-game-id 8888 \
  --token-ttl-sec 120 \
  --token-intent-id intent-local-udp-7777 \
  --token-target-addr 127.0.0.1:7777
```

Production clients should get this token from the backend connect-intent API.
The node-side minting command is only a development/testing helper.

## v0.6.0 UDP Session Data Contract

When a UDP `probe.ok` response returns a `session.session_id`, the node stores a
short-lived UDP session. During this MVP stage the client can send a
`session.data` packet to verify that the client and node agree on the session.

Request:

```json
{
  "type": "session.data",
  "protocol": "xaccel/1",
  "session_id": "ps-udp-1779250000-1-2-3-4-50000-1",
  "client_nonce": "packet-random",
  "payload": "aGVsbG8="
}
```

Response:

```json
{
  "type": "session.data.ok",
  "protocol": "xaccel/1",
  "node_id": 1,
  "node_version": "0.12.0",
  "server_time": 1779250001,
  "transport": "udp",
  "session_id": "ps-udp-1779250000-1-2-3-4-50000-1",
  "client_nonce": "packet-random",
  "status": "echo",
  "payload": "aGVsbG8=",
  "payload_bytes": 5,
  "session": {
    "created_at": 1779250000,
    "expires_at": 1779250030,
    "authenticated": true,
    "intent_id": "intent-local-udp-7777",
    "route_target_addr": "127.0.0.1:7777",
    "user_id": 1001,
    "device_id": "pc-001",
    "game_id": 8888
  }
}
```

Errors return `session.error` with codes such as `missing_session_id`,
`missing_payload`, `invalid_payload`, `session_not_found`, `session_expired`,
or `unsupported_transport`.

Health adds these counters under `sessions`:

```json
{
  "active_udp_sessions": 1,
  "udp_session_rx_packets": 1,
  "udp_session_rx_bytes": 5,
  "udp_session_tx_packets": 1,
  "udp_session_tx_bytes": 260,
  "udp_session_miss": 0,
  "udp_session_expired": 0
}
```

When no target is provided, the `session.data` response is an echo integration
check.

## v0.7.0 UDP Target Relay Contract

Authenticated UDP sessions can include a target endpoint. The node forwards the
decoded payload to that UDP endpoint, waits for one upstream UDP response, and
returns the upstream payload to the client.

Request with `target_host` and `target_port`:

```json
{
  "type": "session.data",
  "protocol": "xaccel/1",
  "session_id": "ps-udp-1779250000-1-2-3-4-50000-1",
  "client_nonce": "packet-random",
  "payload": "aGVsbG8=",
  "target_host": "127.0.0.1",
  "target_port": 7777,
  "response_timeout_ms": 200
}
```

`target_addr` is also accepted when the endpoint already includes a port:

```json
{
  "target_addr": "127.0.0.1:7777"
}
```

Response with an upstream payload:

```json
{
  "type": "session.data.ok",
  "protocol": "xaccel/1",
  "node_version": "0.12.0",
  "transport": "udp",
  "session_id": "ps-udp-1779250000-1-2-3-4-50000-1",
  "status": "forwarded",
  "payload": "dXBzdHJlYW06aGVsbG8=",
  "payload_bytes": 14,
  "request_payload_bytes": 5,
  "target": {
    "address": "127.0.0.1:7777"
  },
  "relay": {
    "mode": "udp_target",
    "timeout_ms": 200,
    "timed_out": false,
    "upstream_tx_bytes": 5,
    "upstream_rx_bytes": 14
  }
}
```

If the upstream endpoint does not respond before `response_timeout_ms`, the node
returns `status = "upstream_timeout"` with an empty payload and increments
`sessions.udp_relay_timeout`.

Target relay currently requires a token-authenticated probe session. Untrusted
or token-missing sessions can still use echo mode but receive
`relay_auth_required` when they attempt target forwarding.

## v0.8.0 Connect-Intent Route Binding

The preferred target relay path is now token-bound. The backend connect-intent
API should mint a short-lived token containing the route selected for this user,
device, game, and node. During probe, the node stores `intent_id` and
`route.target_addr` into the UDP session.

After that, `session.data` does not need client-provided target fields:

```json
{
  "type": "session.data",
  "protocol": "xaccel/1",
  "session_id": "ps-udp-1779250000-1-2-3-4-50000-1",
  "client_nonce": "packet-random",
  "payload": "aGVsbG8=",
  "response_timeout_ms": 200
}
```

The node resolves and forwards to the session-bound `route_target_addr`. If both
the token and request provide a target, the token-bound route wins. This keeps
the production path controlled by backend policy while preserving request-level
targets for standalone development tests.

## v0.9.0 Backend Connect-Intent Mock

`xaccel-backend-mock` is a development-only backend service that implements the
client connect-intent endpoint and signs route-bound `xat.v1` credentials using
the same node secret as `xaccel-node`. This proves the production-shaped flow:

1. Client asks backend for a connect intent.
2. Backend selects a node and route target.
3. Backend returns a short-lived route-bound token.
4. Client probes the node with that token.
5. Node stores the route in the session and forwards `session.data`.

## v0.10.0 Rust MySQL Control API

`xaccel-control-api` is the production-shaped implementation of the same
connect-intent contract. It uses Rust, Axum, SQLx, and MySQL.

The service reads `accel_nodes` and `game_route_rules`, stores issued rows in
`connect_intents`, and returns the same candidate shape as `backend-mock`.
Scheduling is still intentionally small: it chooses an online UDP-capable node
by requested bandwidth quality, route priority, recent `last_seen_at`, and node
id. The next step is to add user entitlement, ISP-aware scheduling, latency
measurements, and load-based selection.

## v0.12.0 Client Probe Tool

`xaccel-client-probe` is an operator-facing diagnostic client. It requests a
connect-intent from `xaccel-control-api`, sends the selected node a UDP probe
with the returned token, reuses the same UDP socket for `session.data`, and
prints a JSON result. This keeps manual node validation close to the future
desktop client flow while avoiding copy/paste of tokens and session ids.

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
  "intent_id": "intent-1001-8888-1779250000-1",
  "ttl_sec": 120,
  "client": {
    "platform": "pc",
    "client_isp": "telecom",
    "client_ip": "x.x.x.x",
    "bandwidth_quality": "fast"
  },
  "candidates": [
    {
      "node_id": 1,
      "area": "HK",
      "tag": "free",
      "host": "1.2.3.5",
      "port": 666,
      "transports": ["udp"],
      "bandwidth_quality": "normal",
      "probe": {
        "udp": true,
        "tcp": true,
        "protocol": "xaccel/1"
      },
      "route": {
        "target_addr": "203.0.113.10:27015",
        "protocol": "udp"
      },
      "credential": {
        "token": "xat.v1.payload.signature",
        "expires_at": 1779250120,
        "intent_id": "intent-1001-8888-1779250000-1",
        "route": {
          "target_addr": "203.0.113.10:27015",
          "protocol": "udp"
        }
      }
    }
  ]
}
```
