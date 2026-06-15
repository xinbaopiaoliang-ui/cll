# 单次加速票据与动态路由策略合同

本文档是业务后台、控制面、客户端加速内核、节点内核的联调合同。目标不是固定转发某一个 `IP:port`，而是让业务后台把本次加速所需的游戏域名、IP、CIDR、端口、端口段、协议、节点选择、权益、设备、风控等信息一次性下发，客户端按策略捕获和转发，节点按签名策略验票和放行。

## 外部调研结论

- Steam 官方端口资料说明，Steam/Steamworks 游戏会涉及多组端口范围，例如游戏流量、匹配、下载、P2P、语音等，不适合建模为一个固定端口。
- Steam Datagram Relay 是 Valve 的游戏中继网络。使用 SDR 的游戏可能不会暴露真实游戏服 IP，而是通过中继、SteamID、FakeIP 或票据访问。
- 永劫无间国际服官方资料能确认存在 NA/EU/AS/SEA 等区服，但未公开稳定、完整的游戏服 IP/端口清单。第三方资料给出的 Steam 版端口也主要落在 Steam 常见端口范围内，因此真实联调必须依赖客户端观测、业务后台配置和动态策略，而不是节点本地写死。

参考资料：

- Steam Required Ports: https://help.steampowered.com/en/faqs/view/2EA8-4D75-DA21-31EB
- Steam Datagram Relay: https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay
- NARAKA 官方国际服 FAQ: https://www.narakathegame.com/news/official/20210811/32172_965598.html
- NARAKA Steam 版第三方端口参考: https://www.purevpn.com/port-forwarding/naraka-bladepoint

## 职责边界

- 业务后台负责：游戏、区服、节点选择、域名、IP、CIDR、端口、端口段、协议、用户、设备、订单、权益、风控、业务会话。
- 控制面负责：节点运维、节点身份、节点密钥、节点健康、版本、重启、升级、短期 token 签发、会话和流量上报。
- 客户端加速内核负责：向业务后台请求加速票据，按票据内的动态路由策略捕获目标流量，把 token 和策略发送给节点。
- 节点内核负责：校验 token、校验路由策略哈希、创建短期会话、校验每个转发目标是否命中策略、转发数据、上报状态。

节点不得加载或缓存游戏目录、区服目录、线路表，也不得判断用户是否已付费。节点只信任签名 token 内的声明，以及被 token 哈希绑定的 `route_policy`。

## 联调总流程

1. 用户在客户端选择游戏、区服或业务后台定义的加速入口。
2. 客户端把本机环境和可观测信息提交给业务后台，例如进程名、平台、客户端 IP、运营商、已解析域名、近期连接目标、Steam/NARAKA 入口。
3. 业务后台根据自身配置和客户端观测结果，决定本次加速的节点和动态路由策略。
4. 业务后台调用控制面 `/api/business/v1/connect-intent`，传入本次会话全部参数。
5. 控制面确认节点可用，生成 `route_policy_hash`，用选定节点的 `node_secret` 签发 `credential.token`，并返回 `accel_ticket`。
6. 客户端向 `accel_ticket.node` 发送 `probe`，携带 token 和 `route_policy`。
7. 节点校验 token、校验 `sha256(route_policy)` 是否等于 token 内 `route_policy_hash`，创建会话。
8. 客户端转发每个游戏数据包时带上真实目标 `target`，节点校验该目标命中 `route_policy.targets[]` 后再转发。

## 业务后台调用控制面

当前控制面临时承担业务签发服务，接口为 `/api/business/v1/connect-intent`。业务后台必须把本次加速策略传给控制面。旧字段 `target_addr` 可保留用于单目标兼容，但正式联调以 `route_policy.targets[]` 为准。

```json
{
  "request_id": "req-20260612-0001",
  "entitlement_id": "vip-order-1001",
  "order_id": "order-1001",
  "subscription_id": "sub-2026",
  "business_session_id": "session-1001",
  "entitlement_verified": true,
  "device_verified": true,
  "entitlement_expires_at": 1781253600,
  "risk_level": "normal",
  "business_trace_id": "trace-20260612-0001",
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 730000,
  "game_key": "naraka_global",
  "region_id": 1,
  "region_name": "International",
  "node_id": 2,
  "platform": "pc",
  "client_isp": "telecom",
  "client_ip": "203.0.113.10",
  "bandwidth_quality": "fast",
  "client_version": "0.1.0",
  "route_policy": {
    "policy_id": "rp-session-1001-1781250000",
    "policy_version": 1,
    "mode": "dynamic_targets",
    "default_protocol": "udp",
    "dns_strategy": "client_observed_then_node_resolve",
    "targets": [
      {
        "target_id": "steam-p2p",
        "purpose": "steam_p2p_or_voice",
        "host_type": "any",
        "ports": [
          { "protocol": "udp", "from": 3478, "to": 3478 },
          { "protocol": "udp", "from": 4379, "to": 4380 },
          { "protocol": "udp", "from": 27014, "to": 27030 }
        ],
        "allow_client_observed_ip": true,
        "required": false
      },
      {
        "target_id": "naraka-gameplay-observed",
        "purpose": "gameplay",
        "host_type": "observed_ip",
        "observed_ips": ["198.51.100.20", "198.51.100.21"],
        "ports": [
          { "protocol": "udp", "from": 27000, "to": 27050 }
        ],
        "allow_client_observed_ip": true,
        "required": true
      },
      {
        "target_id": "naraka-domain",
        "purpose": "login_or_matchmaking",
        "host_type": "domain",
        "host": "example.naraka-game-service.invalid",
        "resolved_ips": ["203.0.113.20"],
        "ports": [
          { "protocol": "tcp", "from": 443, "to": 443 },
          { "protocol": "udp", "from": 27000, "to": 27050 }
        ],
        "resolve_ttl_sec": 60,
        "required": false
      }
    ],
    "capture": {
      "process_names": ["NarakaBladepoint.exe", "NarakaBladepointClient.exe", "steam.exe"],
      "process_match": "any",
      "exclude_private_lan": true,
      "exclude_ports": [
        { "protocol": "tcp", "from": 80, "to": 80, "reason": "download_or_web" },
        { "protocol": "tcp", "from": 443, "to": 443, "reason": "login_only_unless_target_matched" }
      ]
    }
  }
}
```

### 控制面请求必传字段

- `user_id`: 业务用户 ID。
- `device_id`: 已校验的设备 ID。
- `game_id`: 业务游戏 ID，用于归因和 token 声明。
- `node_id`: 业务后台为本次加速选择的控制面节点 ID。
- `route_policy.policy_id`: 本次策略 ID，建议按业务会话生成。
- `route_policy.mode`: 当前正式联调使用 `dynamic_targets`。
- `route_policy.targets`: 本次允许转发的目标集合。
- `route_policy.targets[].ports`: 至少一个协议和端口或端口段。
- `entitlement_verified`: 必须为 `true`。
- `device_verified`: 必须为 `true`。

### 控制面请求可选字段

- `game_key`
- `region_id`
- `region_name`
- `platform`
- `client_isp`
- `client_ip`
- `bandwidth_quality`
- `order_id`
- `subscription_id`
- `business_session_id`
- `entitlement_expires_at`
- `risk_level`
- `business_trace_id`
- `route_policy.targets[].host`
- `route_policy.targets[].resolved_ips`
- `route_policy.targets[].cidrs`
- `route_policy.capture`

## route_policy 字段说明

`route_policy` 是业务后台给客户端和节点的动态放行策略。客户端用它决定哪些包要转发到节点，节点用它决定每个 `session.data` 的目标是否允许。

### RoutePolicy

```json
{
  "policy_id": "rp-session-1001-1781250000",
  "policy_version": 1,
  "mode": "dynamic_targets",
  "default_protocol": "udp",
  "dns_strategy": "client_observed_then_node_resolve",
  "targets": [],
  "capture": {}
}
```

- `policy_id`: 业务后台生成的策略 ID。
- `policy_version`: 策略结构版本，当前为 `1`。
- `mode`: `dynamic_targets` 表示客户端每个转发包都带目标，节点逐包校验。
- `default_protocol`: 未显式指定协议时的默认值，建议为 `udp`。
- `dns_strategy`:
  - `client_observed`: 只信任客户端启动前或运行中观测到的域名和 IP。
  - `node_resolve`: 节点按域名解析，并把解析结果用于校验。
  - `client_observed_then_node_resolve`: 优先使用客户端观测，必要时节点解析补充。
- `targets`: 允许转发的目标集合。
- `capture`: 客户端流量捕获建议，不作为节点放行依据。

### RouteTarget

```json
{
  "target_id": "steam-p2p",
  "purpose": "steam_p2p_or_voice",
  "host_type": "any",
  "host": null,
  "resolved_ips": [],
  "cidrs": [],
  "ports": [
    { "protocol": "udp", "from": 3478, "to": 3478 }
  ],
  "allow_client_observed_ip": true,
  "resolve_ttl_sec": 60,
  "required": false
}
```

- `target_id`: 目标规则 ID。客户端转发时可以带上它，便于节点快速匹配和日志归因。
- `purpose`: 目标用途，例如 `login`、`matchmaking`、`gameplay`、`steam_p2p_or_voice`、`relay`。
- `host_type`:
  - `domain`: `host` 是域名，节点可按策略解析。
  - `ipv4`: `host` 是 IPv4。
  - `ipv6`: `host` 是 IPv6。
  - `cidr`: 使用 `cidrs`。
  - `observed_ip`: 使用客户端或业务后台观测到的 `resolved_ips`。
  - `steam_sdr`: Steam Datagram Relay 或 FakeIP 场景，业务后台应按观测结果和端口段限制。
  - `any`: 不限制 IP，只限制协议和端口。只能用于 Steam P2P/STUN/语音这类明确端口段，并建议设置更短 TTL。
- `host`: 域名或单个 IP。
- `resolved_ips`: 业务后台或客户端观测到的 IP 列表。
- `cidrs`: 允许的 IP 网段。
- `ports`: 协议和端口范围。
- `allow_client_observed_ip`: 客户端是否可在 `probe` 或后续上报中补充观测 IP。
- `resolve_ttl_sec`: 域名解析结果有效期。
- `required`: 是否为启动游戏必须命中的目标。

### PortRange

```json
{
  "protocol": "udp",
  "from": 27000,
  "to": 27050
}
```

`control-api 0.71.1` supports `udp` and `tcp` for per-session ticket
issuance. If `target_addr` is omitted, the control plane chooses a
representative target from `route_policy.targets[]` that matches the selected
protocol and writes it into the compatibility `route.target_addr` field. The
client should still use the full `route_policy` as the actual forwarding
allowlist.

- `protocol`: `udp` 或 `tcp`。当前节点先实现 UDP 转发，合同预留 TCP。
- `from`: 起始端口。
- `to`: 结束端口。单端口时 `from` 和 `to` 相同。

## 控制面返回给业务后台

控制面返回 `accel_ticket`。业务后台可以直接把 `accel_ticket` 透传给客户端，也可以包一层自己的业务响应。

```json
{
  "status": "ok",
  "accel_ticket": {
    "ticket_id": "intent-1001-730000-1781250000-2",
    "ttl_sec": 120,
    "issue_mode": "per_session",
    "client": {
      "user_id": 1001,
      "device_id": "pc-001",
      "game_id": 730000,
      "game_key": "naraka_global",
      "region_id": 1,
      "platform": "pc",
      "client_isp": "telecom",
      "client_ip": "203.0.113.10",
      "bandwidth_quality": "fast"
    },
    "node": {
      "node_id": 2,
      "host": "47.83.160.126",
      "port": 666,
      "area": "HK",
      "tag": "default",
      "transports": ["udp"],
      "bandwidth_quality": "normal"
    },
    "route_policy": {
      "policy_id": "rp-session-1001-1781250000",
      "policy_version": 1,
      "mode": "dynamic_targets",
      "default_protocol": "udp",
      "dns_strategy": "client_observed_then_node_resolve",
      "targets": []
    },
    "credential": {
      "token": "xat.v1.payload.signature",
      "expires_at": 1781250120,
      "intent_id": "intent-1001-730000-1781250000-2",
      "token_type": "xat.v1",
      "signing_alg": "HMAC-SHA256",
      "route_policy_hash": "sha256-base64url-no-pad"
    },
    "auth_context": {
      "entitlement_id": "vip-order-1001",
      "order_id": "order-1001",
      "subscription_id": "sub-2026",
      "business_session_id": "session-1001",
      "entitlement_verified": true,
      "device_verified": true,
      "entitlement_expires_at": 1781253600,
      "risk_level": "normal",
      "business_trace_id": "trace-20260612-0001"
    }
  },
  "connect_intent": {
    "intent_id": "intent-1001-730000-1781250000-2",
    "ttl_sec": 120,
    "issue_mode": "per_session",
    "candidates": []
  },
  "server_time": 1781250000
}
```

## Token 生成规则

token 由控制面生成，业务后台和客户端不需要知道节点密钥。节点使用本机保存的 `node_secret` 验证 token。

### Claims

```json
{
  "node_id": 2,
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 730000,
  "game_key": "naraka_global",
  "region_id": 1,
  "business": {
    "entitlement_id": "vip-order-1001",
    "order_id": "order-1001",
    "subscription_id": "sub-2026",
    "business_session_id": "session-1001",
    "entitlement_verified": true,
    "device_verified": true,
    "entitlement_expires_at": 1781253600,
    "risk_level": "normal",
    "business_trace_id": "trace-20260612-0001"
  },
  "intent_id": "intent-1001-730000-1781250000-2",
  "route_policy_hash": "sha256-base64url-no-pad",
  "route_policy_id": "rp-session-1001-1781250000",
  "expires_at": 1781250120,
  "issued_at": 1781250000,
  "nonce": "1781250000-2-pc-001"
}
```

### 签名算法

当前 token 格式为：

```text
xat.v1.<payload_base64url_no_pad>.<signature_base64url_no_pad>
```

生成步骤：

1. 控制面生成 `claims` JSON。
2. 使用 UTF-8 编码得到 `payload_bytes`。签名和验签只使用这份 payload 原始字节，验签方不得重新格式化 JSON 后再签。
3. `payload_base64url_no_pad = base64url(payload_bytes, no_padding)`。
4. `signing_input = "xat.v1." + payload_base64url_no_pad`。
5. `signature = HMAC-SHA256(node_secret, signing_input)`。
6. `signature_base64url_no_pad = base64url(signature, no_padding)`。
7. `token = signing_input + "." + signature_base64url_no_pad`。

伪代码：

```text
payload = json_utf8(claims)
payload_b64 = base64url_no_pad(payload)
signing_input = "xat.v1." + payload_b64
signature = hmac_sha256(node_secret, signing_input)
token = signing_input + "." + base64url_no_pad(signature)
```

节点验签：

1. 拆分 token，必须得到 4 段，且前两段为 `xat`、`v1`。
2. 用当前节点 `node_secret` 对 `xat.v1.<payload>` 做 HMAC-SHA256。
3. 比较签名，失败返回 `invalid_token`。
4. 解码 payload JSON。
5. 校验 `node_id`、`expires_at`、`device_id`、`user_id`、`game_id`、`region_id`。
6. 计算客户端随 `probe` 发送的 `route_policy` 哈希，必须等于 `claims.route_policy_hash`。
7. 后续每个 `session.data.target` 必须命中 `route_policy.targets[]`。

### route_policy_hash

`route_policy_hash` 用于把大策略绑定到短 token 中，避免 token 过长。

生成规则：

1. 控制面和节点都使用同一份 `route_policy` JSON 原文。
2. 控制面将返回给客户端的 `route_policy` 序列化为 UTF-8 字节。
3. `route_policy_hash = base64url_no_pad(sha256(route_policy_bytes))`。
4. 客户端 `probe` 时必须发送同一份 `route_policy`。
5. 节点计算收到的 `route_policy` 哈希，必须与 token 内声明一致。

为避免跨语言 JSON 字段顺序差异，建议控制面返回 `route_policy_canonical` 字符串或 `route_policy_hash_input` 字段。客户端原样透传给节点，节点用原始字符串计算哈希。

## 客户端发给节点：probe

客户端启动加速会话时，把 token、客户端镜像字段和路由策略一起发给节点。

```json
{
  "type": "probe",
  "protocol": "xaccel/1",
  "client_nonce": "probe-1001-1781250001",
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 730000,
  "region_id": 1,
  "transport": "udp",
  "token": "xat.v1.payload.signature",
  "route_policy": {
    "policy_id": "rp-session-1001-1781250000",
    "policy_version": 1,
    "mode": "dynamic_targets",
    "default_protocol": "udp",
    "targets": []
  }
}
```

节点响应：

```json
{
  "type": "probe.ok",
  "protocol": "xaccel/1",
  "node_id": 2,
  "node_version": "0.39.0",
  "server_time": 1781250001,
  "transport": "udp",
  "requested_transport": "udp",
  "client_nonce": "probe-1001-1781250001",
  "session": {
    "session_id": "ps-udp-1781250001-47-83-160-126-41000-1",
    "status": "ready",
    "ttl_sec": 30,
    "intent_id": "intent-1001-730000-1781250000-2",
    "route_policy_id": "rp-session-1001-1781250000",
    "route_policy_hash": "sha256-base64url-no-pad",
    "auth_required": true,
    "credential_present": true,
    "credential_valid": true,
    "credential_expires_at": 1781250120,
    "user_id": 1001,
    "device_id": "pc-001",
    "game_id": 730000,
    "region_id": 1
  },
  "capabilities": [
    "udp_probe",
    "udp_relay",
    "dynamic_route_policy",
    "domain_target",
    "port_range_target"
  ]
}
```

## 客户端发给节点：session.data

客户端每转发一个数据包，都必须带上真实目标。节点按 `route_policy.targets[]` 校验后转发。

```json
{
  "type": "session.data",
  "protocol": "xaccel/1",
  "session_id": "ps-udp-1781250001-47-83-160-126-41000-1",
  "client_nonce": "data-1001-1781250002",
  "target": {
    "target_id": "naraka-gameplay-observed",
    "protocol": "udp",
    "host": "198.51.100.20",
    "port": 27015,
    "original_domain": null
  },
  "payload": "aGVsbG8=",
  "response_timeout_ms": 500
}
```

节点响应：

```json
{
  "type": "session.data.ok",
  "protocol": "xaccel/1",
  "node_id": 2,
  "node_version": "0.39.0",
  "transport": "udp",
  "session_id": "ps-udp-1781250001-47-83-160-126-41000-1",
  "status": "forwarded",
  "payload": "dXBzdHJlYW06aGVsbG8=",
  "payload_bytes": 14,
  "request_payload_bytes": 5,
  "target": {
    "target_id": "naraka-gameplay-observed",
    "protocol": "udp",
    "address": "198.51.100.20:27015",
    "matched_policy": "rp-session-1001-1781250000"
  },
  "relay": {
    "mode": "udp_target",
    "timeout_ms": 500,
    "timed_out": false,
    "upstream_tx_bytes": 5,
    "upstream_rx_bytes": 14
  }
}
```

节点拒绝示例：

```json
{
  "type": "error",
  "protocol": "xaccel/1",
  "error": {
    "code": "target_not_allowed",
    "message": "target does not match route_policy"
  }
}
```

## 客户端开发要求

客户端可按以下步骤开发：

1. 识别当前加速入口：`game_id`、`game_key`、`region_id`、平台、客户端版本。
2. 识别游戏进程：例如 `NarakaBladepoint.exe`、`steam.exe`，并记录进程 PID。
3. 记录 DNS 结果：域名、解析 IP、TTL、解析时间。
4. 记录连接目标：协议、远端 IP、端口、进程、时间、是否属于游戏进程。
5. 请求业务后台创建加速票据。
6. 收到 `accel_ticket` 后，向节点发送 `probe`。
7. `probe.ok` 后进入转发循环。
8. 对每个待转发包，先在本地匹配 `route_policy.targets[]`，命中后发送 `session.data`。
9. 收到 `target_not_allowed` 时停止转发该目标，并把目标上报业务后台用于补规则。
10. 票据过期前自动续票，续票后重新 `probe` 或更新会话。

客户端必须注意：

- 不要把下载、更新、网页、广告、遥测流量默认纳入游戏转发。
- 域名目标最终会表现为 IP 包，客户端应尽量保留 `original_domain` 与解析结果的对应关系。
- Steam/SDR/P2P 场景可能出现中继 IP、FakeIP 或固定端口段，不能只依赖游戏服 IP。
- `route_policy` 是业务后台配置结果，客户端不能自行扩大范围。
- 明文字段和 token 声明冲突时，节点会拒绝。

## 节点校验要求

节点必须实现以下校验：

- token 格式、签名、过期时间、`node_id`。
- `user_id`、`device_id`、`game_id`、`region_id` 镜像字段。
- `route_policy_hash`。
- `session.data.target.protocol` 必须命中端口规则的协议。
- `session.data.target.port` 必须落在某个 `ports[].from..to` 范围内。
- IP、域名、CIDR、observed IP 必须命中对应 `RouteTarget`。
- 若 `host_type=any`，只能按业务明确允许的端口段放行，并建议记录更高风险日志。
- 不允许客户端在未命中策略时使用任意 `target_addr`。

## v0.70.0 当前实现状态

当前代码已经完成动态策略联调主链路：

- 控制面业务 `connect-intent` 支持 `route_policy.targets[]`、域名、IP、CIDR、observed IP、端口段。
- 控制面会计算 `route_policy_hash`，并把 `route_policy_hash`、`route_policy_id`、可选 `game_key` 签入节点 token。
- `accel_ticket` 会返回 `route_policy`，客户端可以把它带给节点。
- 节点 `probe` 支持接收 `route_policy` 并校验 hash。
- 节点会话会保存 `route_policy`，并对每个 `session.data.target` 做 allowlist 校验。
- `session.data` 支持显式 `target` 对象，节点逐包校验目标后再转发。
- `xaccel-client-probe` 支持从 ticket 中读取 `route_policy`，也支持通过 `--target-host`、`--target-port`、`--target-id` 构造动态目标做联调。
- Apifox 主文件和 v0.70.0 快照已补齐动态路由策略 schema。

仍未进入本阶段代码范围的内容：

- Windows 客户端内核的进程识别、DNS 观测、UDP 包捕获和动态目标匹配。
- TCP 转发。
- Steam SDR/FakeIP 的专用识别和补规则上报闭环。
- 节点侧按策略维度输出更细的流量归因报表。

## 联调优先级

1. 先实现动态策略的 JSON 协议：控制面签发、客户端 probe、节点 hash 校验。
2. 再实现节点按端口段和 IP allowlist 放行 `session.data.target`。
3. 再让客户端探针构造多个目标，验证同一会话内不同 UDP 目标都能转发。
4. 然后接 Steam 端口范围联调。
5. 最后接永劫无间国际服，通过客户端观测域名/IP/端口，把结果回填业务后台配置。
