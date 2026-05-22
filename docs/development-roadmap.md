# Development Roadmap

## P0: Research And Prototype

Done:

- Domain model from rule/server field documents.
- Linux node design.
- One-click installer design.
- GitHub repository and release workflow.

## P1: Node Lifecycle MVP

Done in `v0.1.0`:

- Install through GitHub script.
- Download release artifact and verify sha256.
- Start `xaccel-node` as a systemd service.
- Load config and identity.
- Expose local health endpoint.

## P2: Listener MVP

Done in `v0.2.0`:

- Bind configured TCP `server_ip:server_port`.
- Bind configured UDP `server_ip:server_port`.
- Return simple TCP/UDP probe responses.
- Record basic TCP/UDP counters in `/health`.

Done in `v0.4.0`:

- Keep legacy TCP/UDP `ping` probe compatibility.
- Add JSON `xaccel/1` client probe request and `probe.ok` response.
- Return short-lived probe session ids.
- Record accepted and rejected probe session counters in `/health`.

Done in `v0.5.0`:

- Add `xat.v1` HMAC client token verification.
- Add a development CLI command to mint short-lived client tokens from node identity.
- Reject malformed, expired, mismatched, or incorrectly signed tokens.
- Record auth missing, auth ok, and auth failed counters in `/health`.

Done in `v0.6.0`:

- Store short-lived UDP probe sessions in an in-memory session table.
- Accept UDP `session.data` packets by `session_id`.
- Echo base64 payloads for client integration testing.
- Record active UDP sessions, session rx/tx, missing session, and expired
  session counters in `/health`.

Done in `v0.7.0`:

- Add authenticated UDP target relay for `session.data`.
- Resolve `target_addr` or `target_host` + `target_port` from client packets.
- Return upstream UDP response payloads to the client.
- Keep UDP listener receive loop non-blocking while relay packets wait for
  upstream responses.
- Record UDP relay tx/rx, timeout, and error counters in `/health`.

Next:

- Add structured bind error reporting.
- Add backend-issued client token API.
- Bind UDP target forwarding to backend-issued connect-intents and game rules.

## P3: Control Plane

Done in `v0.3.0`:

- Add optional control-plane report loop.
- Sign node report requests with HMAC-SHA256.
- Report health, listener, traffic, and session snapshots.
- Expose control-plane success/failure state in `/health`.

Goals:

- Implement backend handshake.
- Parse production bootstrap response.
- Fetch node config and hot-apply safe fields.
- Add websocket or long-poll events for drain, config update, and user kick.

## P4: UDP Relay MVP

Goals:

- Bind target mappings from backend connect-intents.
- Add idle timeout and LRU cleanup.
- Count traffic per session.

## P5: Game Acceleration Tunnel

Goals:

- Implement client-to-node tunnel protocol.
- Add QUIC UDP tunnel.
- Add reconnect and keepalive.
- Add basic congestion/loss metrics.

## P6: Production Hardening

Goals:

- Gray release and rollback.
- Multi-ISP IP binding.
- IPv6 support.
- Relay node support.
- User/device auth.
- Prometheus or structured metrics.
