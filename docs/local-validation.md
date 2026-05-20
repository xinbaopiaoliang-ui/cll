# 本地验证流程

本文验证当前研究目录中的一键安装资产和 Rust 节点内核 MVP。Windows 上可以验证 Rust 代码结构；真实 systemd 安装仍以 Linux 为准。

## 研究目录自检

```powershell
& "D:\项目\broad\game-accelerator-research\scripts\validate-research-tree.ps1"
```

自检内容：

- 必要文档存在。
- OpenAPI、DB schema、安装器脚本存在。
- Rust `node-core` 存在。
- Xboard 内没有误创建研究目录。

## Rust 节点内核检查

```powershell
cd D:\项目\broad\game-accelerator-research\node-core
cargo fmt --check
cargo test
cargo run -- --check-config ..\install\config.example.toml
```

运行 daemon：

```powershell
cargo run -- --config ..\install\config.example.toml
```

健康检查：

```powershell
curl http://127.0.0.1:9876/health
```

当前 MVP 只验证生命周期，不做真实流量转发。

## Linux 安装验证

在 Linux 测试机上：

```bash
sudo bash install/install.sh \
  --bootstrap-url https://api.example.com/api/node/v1/bootstrap \
  --bootstrap-token test-token \
  --dry-run
```

正式 bootstrap API 未接入前，不要执行非 `--dry-run` 安装到生产机。

如果要先跑通 systemd 部署链路，可以使用 standalone 模式：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666
```

安装后检查：

```bash
systemctl status xaccel-node
journalctl -u xaccel-node -f
cat /etc/xaccel-node/config.toml
cat /var/lib/xaccel-node/bootstrap-response.json
```

卸载：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/uninstall.sh | sudo bash
```

彻底清理：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/uninstall.sh | sudo bash -s -- --purge
```

## Release 打包验证

在 Linux 或 WSL 中：

```bash
cd game-accelerator-research
bash scripts/package-release.sh
```

输出：

```text
dist/xaccel-node-<version>-linux-<arch>.tar.gz
dist/xaccel-node-<version>-linux-<arch>.sha256
```

## 当前缺口

- 安装脚本仍未解析真实 bootstrap JSON。
- release manifest 还没有自动写入真实 sha256。
- Rust 节点还未实现 control-plane、listener、session、relay。
- Linux systemd 全链路需要在真实服务器验证。
