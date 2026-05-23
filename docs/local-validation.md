# Local Validation

This document verifies the research tree, installer assets, Rust services, and
Linux runtime checks.

## Research Tree

```powershell
& "D:\项目\broad\game-accelerator-research\scripts\validate-research-tree.ps1"
```

The script checks that required docs, installer files, API contracts, DB schema,
and Rust sources exist, and that no research directory was accidentally created
inside Xboard.

## Rust Metadata

On Windows, use the GNU Rust toolchain if the MSVC linker is not installed:

```powershell
cd D:\项目\broad\game-accelerator-research
cargo metadata --manifest-path node-core\Cargo.toml --locked --no-deps --format-version 1
cargo metadata --manifest-path backend-mock\Cargo.toml --locked --no-deps --format-version 1
cargo metadata --manifest-path control-api\Cargo.toml --locked --no-deps --format-version 1
cargo metadata --manifest-path client-probe\Cargo.toml --locked --no-deps --format-version 1
```

On Linux or a complete Rust toolchain:

```bash
cargo test --manifest-path node-core/Cargo.toml --locked
cargo test --manifest-path backend-mock/Cargo.toml --locked
cargo test --manifest-path control-api/Cargo.toml --locked
cargo test --manifest-path client-probe/Cargo.toml --locked
```

## Linux Runtime Check

After installing `v0.17.1`:

```bash
systemctl status xaccel-node
curl http://127.0.0.1:9876/health
ss -lntup | grep ':666'
```

TCP and UDP probe:

```bash
printf 'ping\n' | nc -w 2 103.201.131.99 666
printf 'ping\n' | nc -u -w 2 103.201.131.99 666
```

## Package Release

On Linux or WSL:

```bash
bash scripts/package-release.sh
bash scripts/package-control-api-release.sh
bash scripts/package-client-probe-release.sh
```

Output:

```text
dist/xaccel-node-linux-x86_64.tar.gz
dist/xaccel-node-linux-x86_64.tar.gz.sha256
dist/xaccel-control-api-linux-x86_64.tar.gz
dist/xaccel-control-api-linux-x86_64.tar.gz.sha256
dist/xaccel-client-probe-linux-x86_64.tar.gz
dist/xaccel-client-probe-linux-x86_64.tar.gz.sha256
```

## Backend Connect-Intent Mock

From the repository root:

```bash
export XACCEL_NODE_SECRET='PASTE_NODE_SECRET'
cargo run --manifest-path backend-mock/Cargo.toml -- \
  --listen 127.0.0.1:18080 \
  --node-id 1 \
  --node-host 103.201.131.99 \
  --node-port 666 \
  --target-addr 127.0.0.1:7777
```

## Rust MySQL Control API

The production-shaped connect-intent service is in `control-api`.

```bash
mysql -uroot -p -e "CREATE DATABASE xaccel CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
mysql -uroot -p -e "CREATE USER IF NOT EXISTS 'xaccel'@'%' IDENTIFIED BY 'password';"
mysql -uroot -p -e "GRANT ALL PRIVILEGES ON xaccel.* TO 'xaccel'@'%';"
mysql -uroot -p xaccel < db/schema.sql
mysql -uroot -p xaccel < db/control-api.seed.example.sql

curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/control-api-install.sh | sudo bash -s -- \
  --database-url 'mysql://xaccel:password@127.0.0.1:3306/xaccel' \
  --listen 127.0.0.1:18080
```

Then call:

```bash
curl -fsSL http://127.0.0.1:18080/api/client/v1/connect-intent \
  -H 'Content-Type: application/json' \
  -d '{"user_id":1001,"device_id":"pc-001","game_id":8888,"platform":"pc","client_isp":"telecom","client_ip":"127.0.0.1","bandwidth_quality":"fast"}'
```

Or run the automated client probe:

```bash
cargo run --manifest-path client-probe/Cargo.toml -- \
  --control-url http://127.0.0.1:18080 \
  --user-id 1001 \
  --device-id pc-001 \
  --game-id 8888 \
  --client-isp telecom \
  --client-ip 127.0.0.1 \
  --bandwidth-quality fast
```

## Current Gaps

- The installer still does not parse production bootstrap JSON into all config
  fields.
- The node forwards UDP session data to token-bound route targets, but full game
  tunnel framing is still pending.
- Control-plane config sync, user/device auth, and QUIC tunnel are pending.
