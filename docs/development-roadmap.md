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

Done in `v0.63.0`:

- Add structured listener bind error reporting for node TCP/UDP startup.
- Classify address-in-use, address-not-available, permission, and invalid
  listener failures with operator-facing suggestions in service logs.

Next:

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

Done in `v0.24.0`:

- Rework the Node Management page into a stacked list/detail layout so the
  selected node detail no longer sits in a long right-side column beside empty
  space.
- Spread node health, endpoint/config metrics, realtime counters, recent
  reports, and node actions across a full-width detail panel for quicker
  operations scanning.

Done in `v0.25.0`:

- Refresh the embedded `/admin` dashboard visual system against the newer
  design baseline with a cleaner operations shell, refined summary cards,
  sticky page actions, and subtler technology-console styling.
- Compress the Node Management detail page further by turning the large node
  config form into an on-demand drawer while keeping status, counters, recent
  reports, and install/status actions visible for daily operations.

Done in `v0.25.1`:

- Fix control-api startup schema migration compatibility by avoiding numeric
  decoding of `information_schema.COLUMNS` existence probes on MySQL installs
  that return signed `BIGINT` marker values.

Done in `v0.25.2`:

- Refine the embedded `/admin` page header so the connection status is a compact
  pill and the page title uses a calmer operations-console typography scale.

Done in `v0.25.3`:

- Stabilize the Node Management detail layout by removing the sticky detail
  header, aligning the action forms, and showing Health JSON in a bounded debug
  panel instead of an empty-looking collapsed block.

Done in `v0.25.4`:

- Localize remaining English-facing `/admin` dashboard labels into Chinese,
  including initial summary hints, table metadata, route statuses, node detail
  counters, report statuses, and select option labels while preserving protocol
  names and API enum values.

Done in `v0.26.0`:

- Expose recent `node_audit_logs` entries through the admin node detail API and
  render the latest status-change history in `/admin`, including previous
  status, target status, actor, age, and operator reason.

Done in `v0.26.2`:

- Move the restore-scheduling action into the node list row operations. It is
  available for draining, disabled, or offline nodes, sets the node to the
  offline waiting state with an audit reason, and then lets the next signed
  health report move the node back online for scheduling.

Done in `v0.27.0`:

- Add an operations-page connect-intent diagnostic tool to the embedded
  control dashboard. Operators can submit a user, device, game, ISP, client IP,
  and bandwidth profile, then inspect the selected candidate node, route target,
  transport, credential expiry, token preview, and raw scheduling response.

Done in `v0.28.0`:

- Upgrade the dashboard diagnostic into a full server-side connectivity test.
  The admin API can now run connect-intent, UDP probe, and session.data relay
  checks from the control plane, then return probe latency, relay latency,
  upstream response text, selected route, and exact failure step to `/admin`.

Done in `v0.29.0`:

- Add a first-class game catalog for the control plane. Operators can create,
  edit, enable, disable, filter, and delete game records from `/admin`, while
  route-rule forms can select from the catalog and auto-fill game ID and name.
- Add token-protected admin APIs for `accel_games` and MySQL schema migration
  that seeds the game catalog from existing route rules.

Done in `v0.30.0`:

- Add a business-backend sync API so the business management system can push
  games, game regions, and route rules into the control plane execution copy.
- Add `region_id` support in connect-intent scheduling and `xaccel-client-probe`
  so clients can request region-specific acceleration while still falling back
  to global game routes.

Done in `v0.31.0`:

- Add load-aware connect-intent scheduling metadata. The control plane now
  prefers fresh node reports and lower active session counts when region,
  bandwidth quality, and route priority are otherwise equivalent.
- Include scheduler details in connect-intent and `xaccel-client-probe` output
  so operators can see route priority, latest report age, and session counts.

Done in `v0.63.0`:

- Expand node bootstrap responses with the same runtime network shape used by
  `/api/node/v1/config`.
- Let the Linux installer parse production bootstrap JSON and write identity,
  network, report, and limit settings into `/etc/xaccel-node/config.toml`.
- Keep legacy top-level bootstrap `server_ip` and `server_port` fields for
  older installers while new installers prefer the nested `network` object.

Goals:

- Keep game, region, and route ownership in the business backend. The control
  panel no longer exposes game/route management as a primary menu entry; it
  keeps business sync snapshots and operations fallback visibility only.
- Add nonce replay storage for node report requests.
- Add websocket or long-poll events for drain, config update, and user kick.

Done in `v0.64.0`:

- Add scheduler explanation fields to `connect-intent` candidates. Business
  integrations can now see region match mode, requested quality, quality match,
  report freshness, route priority, live load counters, and selection reasons.
- Show the same scheduler explanation in the control-panel business debug
  result so operators can troubleshoot business/backend/client handoff.

Done in `v0.65.0`:

- Add optional `XACCEL_CLIENT_API_TOKEN` protection for the legacy
  `/api/client/v1/connect-intent` endpoint so clients cannot bypass the
  business backend once formal integration starts.
- Let `xaccel-client-probe 0.33.0` send the same client API token for
  diagnostics.
- Expose `client_api_auth_required` in business status so the business backend
  and operators can verify whether the legacy client path is locked down.
- Extend the installer and integration docs with the client-token rollout path.

Done in `v0.66.0`:

- Require business `connect-intent` calls to carry entitlement and device
  verification results before the control plane signs node credentials.
- Add order ID, subscription ID, business session ID, entitlement expiry,
  risk level, and business trace ID to the business auth context.
- Write the business auth context into `connect-intent` responses and the
  signed node token so later node logs and traffic accounting can be attributed
  back to business orders and sessions.
- Show the auth context in the control-panel business debug result and update
  Apifox/business integration documentation.

Done in `v0.70.0`:

- Define the per-session acceleration ticket contract in
  `docs/per-session-accel-ticket.md`.
- Let business `connect-intent` calls provide `node_id`, `target_addr`,
  `protocol`, and optional region metadata for the current acceleration attempt.
- Sign per-session node credentials without reading `game_route_rules` when the
  business request includes the live route decision.
- Add an `accel_ticket` response envelope that the client can pass to the node.
- Let the node validate `region_id` claim mirrors and keep route forwarding
  bound to the signed token target.
- Let `xaccel-client-probe 0.39.0` run directly from raw or wrapped
  `accel_ticket` JSON for basic client/node diagnostics.
- Upgrade v0.70 from single-target tickets to the dynamic `route_policy`
  contract: domains, IPs, CIDRs, port ranges, observed targets, and
  `route_policy_hash`.
- Let the client send explicit per-packet `target` metadata, and let the node
  verify every target against the signed route policy before relay.

Done in `node-core 0.40.0`:

- Let node probe sessions work across UDP, TCP, and QUIC transports instead of
  storing only UDP probe sessions.
- Add TCP target relay for JSON `session.data` when `target.protocol = "tcp"`.
- Keep TCP client connections open as newline-delimited `xaccel/1` channels so
  a client can send repeated `probe` and `session.data` frames.
- Add the `XAU1` raw UDP tunnel binary frame so game UDP payloads can be
  forwarded without JSON/base64 overhead.
- Add a QUIC tunnel listener on `network.relay_server_port`, reusing the same
  token, session, route-policy, JSON, and raw frame handlers.
- Document the node-facing Windows capture contract in
  `docs/node-kernel-tunnel-protocol.md`.

Next release focus:

- Validate the node tunnel protocols with a real Windows packet capture or
  redirect layer that observes Steam and NARAKA international UDP destinations,
  matches them against business-issued route policies, and sends allowed
  payloads to the node.
- Add traffic attribution reports by business session/order once the tunnel path
  starts carrying real packets.

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
