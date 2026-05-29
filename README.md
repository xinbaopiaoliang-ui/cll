# Game Accelerator Research

This directory is independent from the existing V2board/Xboard project.

The goal is to design a game accelerator in the style of products such as
Leigod: a backend rule and scheduling platform, a client that captures game
traffic according to rules, and Linux-based acceleration nodes that forward
traffic through a self-developed node core.

## Source Documents

- 加速器规则设计.docx
- 服务器管理相关字段.docx

## Design Documents

- [领域模型](docs/domain-model.md)
- [Linux 节点内核设计](docs/linux-node-kernel-design.md)
- [节点配置与接口草案](docs/node-config-and-api.md)
- [一键安装与交付设计](docs/one-click-install-design.md)
- [后台节点安装流程](docs/admin-node-install-flow.md)
- [节点运维、升级与回滚](docs/node-ops-upgrade.md)
- [研发阶段规划](docs/development-roadmap.md)

## Installer Drafts

- [安装器草案](install/install.sh.draft)
- [安装器 MVP 脚本](install/install.sh)
- [卸载脚本草案](install/uninstall.sh)
- [节点配置模板](install/config.example.toml)
- [systemd 服务模板](install/systemd/xaccel-node.service)

## Backend Contracts

- [节点 API OpenAPI 草案](api/openapi-node.yaml)
- [数据库表结构草案](db/schema.sql)
- [Release Manifest 示例](install/release-manifest.example.json)
- [Rust + MySQL connect-intent 控制面](docs/control-api-mysql.md)

## Core Assumptions

- The node core runs on Linux as a userspace daemon first.
- The first production target is high-quality UDP acceleration for games.
- The client decides which game traffic should be accelerated.
- The backend owns game rules, node metadata, scheduling, billing, and health.
- The Linux node owns high-performance forwarding, runtime metrics, relay, and
  protocol adaptation.

## Node Core Prototype

- [Backend connect-intent mock](backend-mock/README.md)
- [Rust MySQL control API](control-api/README.md)
- [Client probe tool](client-probe/README.md)
- [Rust 节点内核 MVP](node-core/README.md)
- [本地验证流程](docs/local-validation.md)
- [Linux 部署流程](docs/deploy-linux.md)
- [Release 打包脚本](scripts/package-release.sh)

## Quick Linux Install

Before deploying, create a GitHub Release by pushing a version tag:

```bash
git tag v0.24.0
git push origin v0.24.0
```

GitHub Actions will build Linux `x86_64` artifacts for `xaccel-node`,
`xaccel-control-api`, and `xaccel-client-probe`. After the release is ready,
use standalone mode:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666
```

Replace `YOUR_SERVER_IP` with the public IP of the Linux server. Current release
automation builds Linux `x86_64` first; `aarch64` packaging is reserved for the
next stage. Version `0.24.0` keeps the legacy TCP/UDP `ping` probe, supports
JSON `xaccel/1` client probe responses, verifies optional `xat.v1` HMAC client
tokens, keeps a short-lived UDP session table, echoes `session.data` packets for
client integration testing, binds backend-style connect-intent routes from
signed tokens, forwards authenticated UDP `session.data` packets to the bound
target address, exposes probe/auth/session/relay counters in `/health`, and
includes the optional HMAC-signed control-plane report loop. It also includes a
development `backend-mock` service that issues production-shaped
`/api/client/v1/connect-intent` responses with route-bound client tokens, plus
a Rust + MySQL `control-api` service and one-click installer for
production-shaped scheduling. The control API can receive signed node runtime
reports, persist health snapshots to MySQL, and expose token-protected admin
node management APIs, create node records, and generate one-time bootstrap
install commands for Linux nodes. It also serves a browser dashboard at `/admin`
backed by token-protected admin APIs for live node visibility, node creation,
status changes, install commands, and config edits. Nodes can poll the signed
`/api/node/v1/config` endpoint and hot-apply safe network metadata changes;
listener endpoint changes are surfaced as `restart_required` until the systemd
service is restarted. Node installs default to binding the local listener on
`0.0.0.0` while keeping the public `server_ip` for client scheduling, which
supports cloud servers whose public IP is NATed. Pulled configs are written back
to the local node TOML so endpoint changes can take effect after a service
restart. Nodes also perform a signed startup handshake with the control plane so
the backend can immediately record node version, boot instance, last_seen, and
current config revision before the first periodic report. The dashboard includes
CRUD for `game_route_rules`, letting operators create, edit, disable, and delete
game-to-node target mappings without direct MySQL access. Version `0.24.0`
also reorganizes the node management workspace so the node list and selected
node detail use the full page width, with status, counters, recent reports, and
operations grouped into compact horizontal sections. Version `0.23.0` adds
route-rule game names across MySQL, the admin API, OpenAPI, and `/admin` so
operators can identify games without remembering numeric IDs. Version
`0.22.0` redesigned `/admin` into a modern management console with login,
sidebar menus, overview, node management, route management, and operations
workspaces. The
`xaccel-client-probe` binary automates the full connect-intent, UDP probe, and
session relay validation flow.
Linux release binaries are built with musl so older glibc distributions can run
the installer output without requiring a system libc upgrade.
Standalone mode leaves backend reporting disabled unless `--enable-control-plane`
is passed with a real backend URL.
