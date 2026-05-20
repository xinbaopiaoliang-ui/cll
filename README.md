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

## Core Assumptions

- The node core runs on Linux as a userspace daemon first.
- The first production target is high-quality UDP acceleration for games.
- The client decides which game traffic should be accelerated.
- The backend owns game rules, node metadata, scheduling, billing, and health.
- The Linux node owns high-performance forwarding, runtime metrics, relay, and
  protocol adaptation.

## Node Core Prototype

- [Rust 节点内核 MVP](node-core/README.md)
- [本地验证流程](docs/local-validation.md)
- [Release 打包脚本](scripts/package-release.sh)

## Quick Linux Install

Before the backend bootstrap API exists, use standalone mode:

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666
```

This installs a placeholder `xaccel-node` service to verify the Linux systemd
deployment path. Replace `YOUR_SERVER_IP` with the public IP of the Linux
server.
