# 业务后台对接控制面

本文档描述业务管理后台如何接入 XAccel 控制面。业务后台仍然负责用户、订单、套餐、权益、游戏配置的主数据；控制面只保存节点调度和执行所需的副本。

## 对接边界

```text
客户端 -> 业务后台 -> 控制面 -> 节点
```

- 客户端不直接持有控制面管理 token。
- 业务后台先校验用户登录、会员套餐、设备限制和游戏权限。
- 校验通过后，业务后台调用控制面的业务 API 生成 `connect-intent`。
- 客户端拿到节点地址、token 和 route 信息后，再和节点通信。

## 职责边界记录

游戏管理和游戏路由的主入口放在业务后台，而不是控制面板。

- 业务后台负责主数据：游戏、区服、套餐、用户、订单、权益、运营策略。
- 控制面负责执行副本：节点状态、调度、路由下发、健康检查、远程运维、操作日志。
- 控制面不再把游戏管理、游戏路由放在主菜单里，避免日常运营绕过业务后台。
- 控制面里的游戏数据定位为“业务游戏同步快照”，主要用于排查 `sync-catalog` 是否同步成功。
- 控制面里的路由数据定位为“路由运维兜底快照”，用于业务后台故障、临时救急、节点转发排查，不作为日常配置主入口。
- 客户端不直接访问控制面管理接口，业务后台完成权益判断后再调用控制面签发加速意图。

后续完善方向：业务后台完成游戏、区服、线路的新增、编辑、批量管理；控制面只提供业务同步 API、调度结果和节点运维入口。

## 控制面安装参数

控制面需要配置业务 API token：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/control-api-install.sh | sudo bash -s -- \
  --database-url 'mysql://xaccel:xaccel_password@127.0.0.1:3306/xaccel' \
  --listen 0.0.0.0:18080 \
  --public-base-url http://103.201.131.99:18080 \
  --business-sync-token '替换成业务后台专用Token'
```

服务器上可查看当前配置：

```bash
sudo sed -n "s/^XACCEL_BUSINESS_SYNC_TOKEN='\(.*\)'$/\1/p" /etc/xaccel-control-api/control-api.env
```

控制台也可以查看和修改：登录 `/admin` 后进入“系统设置 -> 业务后台对接 Token”。明文、复制、生成、保存和清空只对超级管理员开放，运维和只读账号只能确认是否已配置。后台保存后会写入 `/etc/xaccel-control-api/control-api.env`，并立即用于业务 API 鉴权。

业务 API 支持两种鉴权头，推荐 `Authorization`：

```http
Authorization: Bearer <XACCEL_BUSINESS_SYNC_TOKEN>
```

兼容头：

```http
X-Business-Sync-Token: <XACCEL_BUSINESS_SYNC_TOKEN>
```

## 1. 业务 API 状态检查

用于业务后台启动时或定时探活。

Apifox 可直接导入 OpenAPI 文件：[apifox-business-api.openapi.json](apifox-business-api.openapi.json)。这个文件已经补齐 Header、请求体、响应体、枚举、默认值、示例和每个字段的业务说明。导入后在环境变量里配置：

- `baseUrl`：例如 `http://103.201.131.99:18080`
- `XACCEL_BUSINESS_SYNC_TOKEN`：系统设置里保存的业务后台 Token

```bash
curl -fsSL http://103.201.131.99:18080/api/business/v1/status \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}"
```

返回示例：

```json
{
  "status": "ok",
  "version": "0.56.0",
  "catalog_owner": "business_backend",
  "control_role": "node_operations",
  "business_api_enabled": true,
  "nodes_total": 2,
  "nodes_online": 2,
  "games_enabled": 1,
  "routes_enabled": 2,
  "server_time": 1781070000
}
```

其中 `catalog_owner=business_backend` 表示游戏、区服、线路主数据由业务后台维护；`control_role=node_operations` 表示控制面只负责节点、调度和执行副本。

## 2. 同步游戏、区服和路由

业务后台把游戏和路由执行副本同步到控制面。建议业务后台保存自己的 `external_id`，后续修改同一路由时使用同一个 `external_id`。

```bash
curl -fsSL -X POST http://103.201.131.99:18080/api/business/v1/sync-catalog \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{
    "source": "business-admin",
    "revision": "2026-06-10T11:40:00+08:00",
    "games": [
      {
        "game_id": 8888,
        "name": "本地 UDP 测试",
        "platform": "pc",
        "category": "test",
        "status": "enabled"
      }
    ],
    "regions": [
      {
        "game_id": 8888,
        "region_id": 1,
        "name": "默认区服",
        "area": "HK",
        "status": "enabled"
      }
    ],
    "route_rules": [
      {
        "external_id": "route-8888-hk-node2",
        "game_id": 8888,
        "game_name": "本地 UDP 测试",
        "region_id": 1,
        "region_name": "默认区服",
        "node_id": 2,
        "target_addr": "127.0.0.1:7777",
        "protocol": "udp",
        "priority": 10,
        "status": "enabled"
      }
    ]
  }'
```

返回示例：

```json
{
  "status": "ok",
  "source": "business-admin",
  "revision": "2026-06-10T11:40:00+08:00",
  "games_upserted": 1,
  "regions_upserted": 1,
  "route_rules_upserted": 1,
  "server_time": 1781070000
}
```

## 3. 业务后台签发加速意图

业务后台完成用户权益校验后调用此接口。该接口会返回节点候选、节点 token 和路由目标。

```bash
curl -fsSL -X POST http://103.201.131.99:18080/api/business/v1/connect-intent \
  -H "Authorization: Bearer ${XACCEL_BUSINESS_SYNC_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{
    "request_id": "req-20260610-0001",
    "entitlement_id": "vip-order-1001",
    "user_id": 1001,
    "device_id": "pc-001",
    "game_id": 8888,
    "region_id": 1,
    "platform": "pc",
    "client_isp": "telecom",
    "client_ip": "127.0.0.1",
    "bandwidth_quality": "fast",
    "client_version": "0.1.0"
  }'
```

返回示例：

```json
{
  "status": "ok",
  "request_id": "req-20260610-0001",
  "entitlement_id": "vip-order-1001",
  "client_version": "0.1.0",
  "connect_intent": {
    "intent_id": "intent-1001-8888-1781070000-2",
    "ttl_sec": 120,
    "client": {
      "platform": "pc",
      "client_isp": "telecom",
      "client_ip": "127.0.0.1",
      "bandwidth_quality": "fast",
      "region_id": 1
    },
    "candidates": [
      {
        "node_id": 2,
        "host": "47.83.160.126",
        "port": 666,
        "transports": ["udp"],
        "route": {
          "target_addr": "127.0.0.1:7777",
          "protocol": "udp",
          "region_id": 1,
          "region_name": "默认区服"
        },
        "credential": {
          "token": "xat.v1.xxx",
          "expires_at": 1781070120,
          "intent_id": "intent-1001-8888-1781070000-2",
          "route": {
            "target_addr": "127.0.0.1:7777",
            "protocol": "udp"
          }
        }
      }
    ]
  },
  "server_time": 1781070000
}
```

客户端需要使用：

- `connect_intent.candidates[0].host`
- `connect_intent.candidates[0].port`
- `connect_intent.candidates[0].credential.token`
- `connect_intent.candidates[0].route`

## 客户端和节点通信流程

```text
1. 客户端登录业务后台
2. 客户端选择游戏和区服
3. 业务后台校验用户权益
4. 业务后台调用 /api/business/v1/connect-intent
5. 客户端拿到节点地址和 token
6. 客户端向节点 UDP probe
7. 节点校验 token，创建 session
8. 客户端发送 session.data
9. 节点转发到游戏目标地址
10. 节点把游戏服务器响应返回客户端
```

## 错误码约定

常见错误：

- `business_sync_disabled`：控制面没有配置业务 token。
- `business_sync_auth_required`：业务后台没有传 token。
- `business_sync_auth_failed`：业务 token 错误。
- `invalid_user`：`user_id` 非法。
- `invalid_game`：`game_id` 非法。
- `invalid_region`：`region_id` 非法。
- `invalid_quality`：`bandwidth_quality` 不是 `fast`、`normal`、`slow`。
- `no_candidate`：没有可用节点或没有启用路由。

业务后台遇到 `no_candidate` 时，不应该让客户端继续连接节点，应提示当前游戏或区服暂无可用线路。

## 联调检查清单

1. 控制面 `/health` 返回 `ready`。
2. `/api/business/v1/status` 返回 `status=ok`。
3. 至少有 1 个节点在线。
4. 至少有 1 条 `enabled` 游戏路由。
5. 业务后台能成功同步游戏和路由。
6. 业务后台能拿到 `connect_intent.candidates[0].credential.token`。
7. 客户端使用 token 能向节点 probe 成功。
8. 客户端 `session.data` 能转发到目标 UDP 服务。
