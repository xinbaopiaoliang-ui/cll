# 研发阶段规划

## P0：研究与原型

目标：

- 确定游戏规则、服务器字段、节点配置和客户端连接意图。
- 完成 Linux 节点内核工程骨架。
- 完成本地回环压测工具。

产物：

- `xaccel-node` 空 daemon。
- 配置加载、日志、metrics、优雅退出。
- UDP echo tunnel 原型。

## P1：MVP 加速链路

目标：

- GitHub Actions 自动构建 Linux `x86_64` release。
- 一键安装器默认下载 release 并校验 sha256。
- 实现 QUIC UDP tunnel。
- 支持后台拉配置。
- 支持节点上报状态和流量。
- 支持客户端基于候选节点连接。

验收：

- 单节点能稳定转发 UDP 游戏流量。
- 支持 1 万级 UDP session。
- 热更新配置不重启进程。

## P2：调度与线路质量

目标：

- 接入 `bandwidth_quality`、`area`、`tag`。
- 支持电信/移动/联通入口 IP。
- 实现 RTT、丢包、抖动采样。
- 后台按客户端运营商返回候选节点。

验收：

- 客户端能自动选择低延迟节点。
- 节点异常后可摘流。
- `disable_quic` 节点不会被选为 UDP 首选。

## P3：中继链路

目标：

- 支持 `relay_server_ip`、`relay_server_port`。
- 入口节点和出口节点共享 session id。
- 上报入口、出口、中继链路质量。

验收：

- 二段转发可用。
- 流量能正确归因到用户、设备、游戏和节点。
- 中继异常可回退直连或其他候选。

## P4：协议增强

目标：

- 支持 WireGuard userspace。
- 支持 TCP/TLS tunnel。
- 根据业务需要扩展 Hysteria2/TUIC 风格参数。

验收：

- 弱网 UDP 有明显改善。
- TCP 登录、平台、网页流量稳定。
- 移动网络 NAT 变化后连接恢复时间可控。

## P5：性能增强

目标：

- 多线程 sharding。
- `SO_REUSEPORT` 多 listener。
- eBPF/tc/XDP 辅助观测或快速丢弃异常流量。
- Prometheus 指标和火焰图分析。

验收：

- 达到生产目标 QPS、吞吐和连接数。
- CPU 利用率、锁竞争、内存增长可解释。
- 故障时能降级，而不是整体崩溃。

## 关键风险

- 客户端规则误判会导致非游戏流量进入通道。
- `cmd_exec` 风险高，必须做白名单和用户确认。
- `fake_ping_value` 不建议改网络包，只做 UI 展示或业务侧显示。
- 多运营商 IP 必须确认真实绑定在 Linux 网卡上。
- QUIC 在部分网络可能被限速，需要 TCP/TLS 或 WireGuard 兜底。
