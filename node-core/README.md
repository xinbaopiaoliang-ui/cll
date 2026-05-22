# xaccel-node MVP

This is the first runnable skeleton of the Linux acceleration node daemon.

It intentionally does not implement game traffic forwarding yet. The goal of
this MVP is to verify the node lifecycle:

- Load `/etc/xaccel-node/config.toml`.
- Load installer identity/bootstrap state.
- Expose a local health endpoint.
- Listen on the configured TCP/UDP server endpoint.
- Record basic TCP/UDP counters.
- Return legacy `ping` probe responses and structured `xaccel/1` probe
  sessions.
- Verify optional `xat.v1` HMAC client tokens.
- Keep short-lived UDP probe sessions and echo `session.data` payloads for
  client integration testing.
- Forward authenticated UDP `session.data` payloads to a target UDP endpoint.
- Optionally post HMAC-signed health reports to the backend.
- Support `--check-config` for installer validation.
- Provide a stable place to add config sync, session, and relay modules.

## Local Run

```bash
cargo run -- --config ../install/config.example.toml
```

Health:

```bash
curl http://127.0.0.1:9876/health
```

Config check:

```bash
cargo run -- --check-config ../install/config.example.toml
```

Generate a development client token from local node identity:

```bash
cargo run -- --config ../install/config.example.toml \
  --make-client-token \
  --token-user-id 1001 \
  --token-device-id pc-001 \
  --token-game-id 8888
```

## Linux Install Shape

The installer eventually places this binary at:

```text
/usr/local/bin/xaccel-node
```

and starts it through:

```text
/etc/systemd/system/xaccel-node.service
```
