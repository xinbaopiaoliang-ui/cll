# XAccel Client Probe

`xaccel-client-probe` is a development client for checking the production-shaped
connect path:

1. request `POST /api/client/v1/connect-intent` from `xaccel-control-api`;
2. send a UDP `probe` packet to the selected node;
3. send one JSON `session.data` packet or one raw UDP `XAU1` tunnel frame
   through the same socket;
4. print a JSON summary with latency, node, route, and relay result.

Version `0.39.0` supports `--session-data-mode raw-udp` for the node
`XAU1` tunnel frame. Version `0.38.0` supports `--accel-ticket-file` and
`--accel-ticket-json` for
the per-session acceleration ticket path, including dynamic `route_policy`
probe and explicit UDP/TCP `session.data.target` relay checks. Version `0.33.0`
supports `--client-api-token` for control panels that protect the legacy direct
client API with `XACCEL_CLIENT_API_TOKEN`.

## Example

```bash
xaccel-client-probe \
  --control-url http://127.0.0.1:18080 \
  --client-api-token "${XACCEL_CLIENT_API_TOKEN}" \
  --user-id 1001 \
  --device-id pc-001 \
  --game-id 8888 \
  --region-id 1 \
  --client-isp telecom \
  --client-ip 127.0.0.1 \
  --bandwidth-quality fast \
  --payload hello
```

When the business backend has already returned an `accel_ticket`, skip legacy
control-plane scheduling and run directly from the ticket JSON:

```bash
xaccel-client-probe \
  --accel-ticket-file ./accel-ticket.json \
  --target-host 198.51.100.20 \
  --target-port 27015 \
  --target-id naraka-gameplay-observed \
  --payload hello
```

Raw UDP tunnel mode:

```bash
xaccel-client-probe \
  --accel-ticket-file ./accel-ticket.json \
  --target-host 198.51.100.20 \
  --target-port 27015 \
  --target-id naraka-gameplay-observed \
  --target-protocol udp \
  --session-data-mode raw-udp \
  --payload hello
```

Raw UDP mode uses the node core's built-in raw relay timeout; the `XAU1` frame
does not carry `--response-timeout-ms`.

The file can contain either the raw `accel_ticket`, the business API response
with an `accel_ticket` field, or the admin debug response with
`result.accel_ticket`. If the ticket contains a concrete target for
`--target-protocol` in `route_policy.targets[]`, the probe can derive the target
automatically. Use `--target-host` and `--target-port` when the policy uses
`host_type=any` or when you want to test a specific observed target.

Expected result:

```json
{
  "status": "ok",
  "node": {
    "address": "103.201.131.99:666",
    "scheduler": {
      "route_priority": 10,
      "latest_active_sessions": 0,
      "report_fresh": true
    }
  },
  "probe": {
    "credential_valid": true
  },
  "session_data": {
    "status": "forwarded"
  }
}
```

Use `--region-id` to verify a region-specific route selected by
`connect-intent`. The `node.scheduler` block explains the selected route
priority, latest report freshness, and current session load used by the control
plane. Use `--skip-session-data` when you only want to validate token issuance
and node authentication. Use `--compact` when scripts need a single-line JSON
result.
