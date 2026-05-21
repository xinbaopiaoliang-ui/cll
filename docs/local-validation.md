# Local Validation

This document verifies the research tree, installer assets, and Rust node MVP.

## Research Tree

```powershell
& "D:\项目\broad\game-accelerator-research\scripts\validate-research-tree.ps1"
```

The script checks that required docs, installer files, API contracts, DB schema,
and Rust sources exist, and that no research directory was accidentally created
inside Xboard.

## Rust Metadata

On this Windows machine, `cargo test` may fail without the MSVC linker. The
lightweight check is:

```powershell
cd D:\项目\broad\game-accelerator-research\node-core
cargo metadata --no-deps --format-version 1
```

On Linux or a complete Rust toolchain:

```bash
cd node-core
cargo fmt --check
cargo test --locked
cargo run -- --check-config ../install/config.example.toml
```

## Linux Runtime Check

After installing `v0.2.0`:

```bash
systemctl status xaccel-node
curl http://127.0.0.1:9876/health
ss -lntup | grep ':666'
```

TCP probe:

```bash
printf 'ping\n' | nc -w 2 103.201.131.99 666
```

UDP probe:

```bash
printf 'ping\n' | nc -u -w 2 103.201.131.99 666
```

Health should show:

```json
{
  "status": "ready",
  "listeners": {
    "udp_listening": true,
    "tcp_listening": true
  },
  "traffic": {
    "udp_rx_packets": 1,
    "tcp_accepted": 1
  }
}
```

## Package Release

On Linux or WSL:

```bash
bash scripts/package-release.sh
```

Output:

```text
dist/xaccel-node-linux-x86_64.tar.gz
dist/xaccel-node-linux-x86_64.tar.gz.sha256
```

## Current Gaps

- The installer still does not parse production bootstrap JSON into all config
  fields.
- The node has basic TCP/UDP listener counters, but no real game forwarding.
- Control-plane sync, relay, user/device auth, and QUIC tunnel are pending.

