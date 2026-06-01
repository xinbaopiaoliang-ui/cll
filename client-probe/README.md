# XAccel Client Probe

`xaccel-client-probe` is a development client for checking the production-shaped
connect path:

1. request `POST /api/client/v1/connect-intent` from `xaccel-control-api`;
2. send a UDP `probe` packet to the selected node;
3. send one UDP `session.data` packet through the same socket;
4. print a JSON summary with latency, node, route, and relay result.

## Example

```bash
xaccel-client-probe \
  --control-url http://127.0.0.1:18080 \
  --user-id 1001 \
  --device-id pc-001 \
  --game-id 8888 \
  --region-id 1 \
  --client-isp telecom \
  --client-ip 127.0.0.1 \
  --bandwidth-quality fast \
  --payload hello
```

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
