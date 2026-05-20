# Linux 部署流程

当前阶段已经具备 Linux 一键部署链路，但节点内核仍是 MVP：它能启动、读取配置、提供健康接口，尚未实现真实游戏转发。

## 1. 创建 GitHub Release

在本地仓库执行：

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions 会自动构建并发布：

```text
xaccel-node-linux-x86_64.tar.gz
xaccel-node-linux-x86_64.tar.gz.sha256
```

进入 GitHub 仓库的 `Actions` 页面，确认 `Release xaccel-node` 工作流成功。

## 2. 在 Linux 服务器安装

把 `YOUR_SERVER_IP` 换成服务器公网 IP：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666
```

如果服务器使用防火墙，并希望安装器尝试开放端口：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666 \
  --open-firewall
```

## 3. 检查服务

```bash
systemctl status xaccel-node
journalctl -u xaccel-node -f
```

检查配置和身份文件：

```bash
cat /etc/xaccel-node/config.toml
sudo cat /var/lib/xaccel-node/bootstrap-response.json
sudo cat /var/lib/xaccel-node/identity.json
```

检查健康接口：

```bash
curl http://127.0.0.1:9876/health
```

## 4. 没有 release 时的安装器测试

如果 GitHub Release 还没构建成功，只想验证 systemd 安装流程，可以显式允许占位服务：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/install.sh | sudo bash -s -- \
  --standalone \
  --node-id 1 \
  --panel-url https://api.example.com \
  --server-ip YOUR_SERVER_IP \
  --server-port 666 \
  --allow-placeholder
```

不建议在正式测试时使用 `--allow-placeholder`。

## 5. 卸载

保留数据和日志：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/uninstall.sh | sudo bash
```

彻底清理：

```bash
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/uninstall.sh | sudo bash -s -- --purge
```

## 6. 当前限制

- 当前 GitHub Actions 只构建 Linux `x86_64`。
- `xaccel-node` 仍是节点生命周期 MVP，不做真实游戏加速转发。
- 后台 bootstrap API 完成后，应从 `--standalone` 切换到 `--bootstrap-url + --bootstrap-token`。

