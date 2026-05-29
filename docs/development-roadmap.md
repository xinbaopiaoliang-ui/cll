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

Done in `v0.8.0`:

- Extend `xat.v1` token claims with `intent_id` and `route.target_addr`.
- Add development CLI flags for minting route-bound client tokens.
- Bind token route targets to UDP probe sessions.
- Let `session.data` forward to the session-bound route without client-provided
  target fields.
- Prefer token-bound routes over client-provided development targets.

Done in `v0.9.0`:

- Add `xaccel-backend-mock` as a standalone development backend service.
- Implement `POST /api/client/v1/connect-intent` for issuing candidate nodes.
- Sign route-bound `xat.v1` client tokens from backend-side node secrets.
- Add backend mock tests and release workflow coverage.
- Document the client-to-backend-to-node connect-intent flow.

Done in `v0.10.0`:

- Add `xaccel-control-api`, a Rust backend service for connect-intent.
- Use Axum for HTTP and SQLx with MySQL for data access.
- Select online nodes and game route rules from MySQL.
- Store issued connect intents for audit and future billing.
- Keep `xat.v1` token signing compatible with `xaccel-node`.

Done in `v0.11.0`:

- Package `xaccel-control-api` as a Linux release artifact.
- Add one-click control-api installer and uninstall script.
- Add systemd service template and secure environment file handling.
- Publish node and control-api binaries in the same GitHub Release.
- Document same-server deployment with `xaccel-node`, MySQL, and control-api.

Done in `v0.12.0`:

- Add `xaccel-client-probe`, a Rust CLI diagnostic client.
- Automate connect-intent, UDP probe, and session.data relay validation.
- Reuse one UDP socket for probe and session data to model client behavior.
- Package the client probe binary in GitHub Releases.
- Document the operator-facing client probe workflow.

Done in `v0.16.1`:

- Add `network.listen_ip` so nodes can bind `0.0.0.0` while keeping public
  `server_ip` for scheduling.
- Default the Linux installer to `listen_ip = "0.0.0.0"` for NATed cloud
  public IP environments.

Done in `v0.16.2`:

- Build Linux release binaries with `x86_64-unknown-linux-musl` to support
  older glibc servers without OS upgrades.

Next:

- Add structured bind error reporting.
- Add user entitlement checks before issuing connect-intents.
- Add production scheduler policy for ISP, region, latency, and node load.
- Add API authentication for clients.

## P3: Control Plane

Done in `v0.3.0`:

- Add optional control-plane report loop.
- Sign node report requests with HMAC-SHA256.
- Report health, listener, traffic, and session snapshots.
- Expose control-plane success/failure state in `/health`.

Done in `v0.13.0`:

- Add `POST /api/node/v1/report` to `xaccel-control-api`.
- Verify node report HMAC headers against each node secret in MySQL.
- Persist raw health snapshots into `node_runtime_reports`.
- Update `accel_nodes.last_report_at`, `last_seen_at`, `kernel_version`, and
  runtime status from signed reports.
- Keep standalone reinstall identity data consistent when changing panel URLs.

Done in `v0.14.1`:

- Add token-protected admin node list and detail endpoints.
- Add admin node status update endpoint for draining, disabling, and recovery
  workflows.
- Store admin status changes in `node_audit_logs`.
- Generate and persist an admin token during control-api one-click install.

Done in `v0.15.0`:

- Add admin bootstrap-token generation endpoint with one-line install command.
- Implement `/api/node/v1/bootstrap` token exchange backed by MySQL.
- Let the Linux installer parse bootstrap responses and write identity,
  network, and control-plane config without standalone parameters.

Done in `v0.16.0`:

- Add admin node creation endpoint so panels can create `accel_nodes` records
  without manual seed SQL.
- Validate node endpoint, quality, relay, area, tag, and operator IP fields.
- Return the created node in the same shape used by node list/detail APIs.

Done in `v0.17.0`:

- Add an embedded `/admin` dashboard to `xaccel-control-api`.
- Show node status, endpoint, versions, report age, listener state, traffic,
  sessions, and recent report JSON through existing admin APIs.

Done in `v0.17.1`:

- Refine the `/admin` dashboard for operations use: compact header, human-readable
  refresh time, localized node status, stale-report highlighting, and collapsed
  Health JSON.

Done in `v0.18.0`:

- Add a dashboard login screen backed by the existing admin bearer token.
- Add admin UI flows for creating nodes, changing node status, generating
  bootstrap install commands, and copying node install commands.

Done in `v0.19.0`:

- Add signed `/api/node/v1/config` for node config downlink.
- Add admin PATCH node config API and dashboard edit form.
- Add node-side config polling, config_revision tracking, and hot-apply for
  safe network metadata fields.
- Mark listener endpoint changes as `restart_required` in health until the node
  service restarts.
- Persist pulled network config back to the local TOML so restart-required
  endpoint changes can take effect after `systemctl restart xaccel-node`.

Done in `v0.20.0`:

- Implement signed node startup handshake at `/api/node/v1/handshake`.
- Update node `last_seen_at`, `kernel_version`, and config revision during
  handshake before the first periodic report arrives.
- Expose handshake success/failure counters and last HTTP status in node
  `/health`.

Done in `v0.21.0`:

- Add admin CRUD APIs for `game_route_rules`.
- Add dashboard list, create, edit, enable/disable, and delete flows for game
  route rules.
- Document the route-rule admin API in the OpenAPI spec and control API README.

Done in `v0.22.0`:

- Redesign `/admin` as a modern management console with a persistent sidebar,
  top action bar, overview, node management, game route management, and
  operations workspaces.
- Refresh the login screen and dashboard visual system for a more polished
  technology-ops product feel.
- Keep the existing token-protected admin APIs and route-rule/node workflows
  wired into the new UI shell.

Done in `v0.23.0`:

- Add `game_name` to game route rules in MySQL, admin APIs, OpenAPI, and the
  embedded control-panel UI.
- Run a startup schema migration so existing control-plane databases receive
  the new `game_route_rules.game_name` column automatically.
- Display route rows as game name plus game ID for easier operations work.

Goals:

- Add nonce replay storage for node report requests.
- Parse production bootstrap response.
- Add websocket or long-poll events for drain, config update, and user kick.

## P4: UDP Relay MVP

Goals:

- Fetch target mappings from backend connect-intents.
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
