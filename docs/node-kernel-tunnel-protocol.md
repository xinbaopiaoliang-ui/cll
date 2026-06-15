# Node Kernel Tunnel Protocol

This document describes the node-facing protocol implemented by
`xaccel-node 0.40.0`. It is the contract a Windows packet-capture layer or any
other client-side accelerator can use when talking to the node.

## Transports

- UDP listener: `network.listen_ip:network.server_port`.
- TCP listener: `network.listen_ip:network.server_port`.
- QUIC listener: `network.relay_server_ip:network.relay_server_port`, or
  `network.listen_ip:network.relay_server_port` when `relay_server_ip` is empty.

QUIC starts only when:

- `network.disable_quic = false`
- `network.relay_server_port` is set to a non-zero port
- `network.relay_server_port != network.server_port`

The QUIC listener uses a self-signed TLS certificate for the current
integration build. Production clients must pin or trust the expected node
certificate model before public release.

## JSON Channel

TCP and QUIC support a long-lived channel. Each request is one frame:

```text
JSON + "\n"
```

For TCP, the node keeps reading newline-delimited frames until the connection
closes. For QUIC, each bidirectional stream carries one request and one
response, while the QUIC connection itself stays open and can carry many
streams.

The existing UDP listener also accepts the same JSON payload in one datagram.

## Probe

Request:

```json
{
  "type": "probe",
  "protocol": "xaccel/1",
  "client_nonce": "probe-1001-1",
  "user_id": 1001,
  "device_id": "pc-001",
  "game_id": 8888,
  "region_id": 1,
  "transport": "quic",
  "token": "xat.v1.payload.signature",
  "route_policy": {}
}
```

The node validates `xat.v1`, verifies `route_policy_hash` when present, then
returns a short-lived `session_id`. The same `session_id` can be used by JSON
`session.data` or the raw UDP tunnel frame.

## JSON Session Data

Request:

```json
{
  "type": "session.data",
  "protocol": "xaccel/1",
  "session_id": "ps-quic-...",
  "client_nonce": "data-1001-1",
  "target": {
    "target_id": "gameplay",
    "protocol": "udp",
    "host": "198.51.100.20",
    "port": 27015
  },
  "payload": "base64-game-packet",
  "response_timeout_ms": 500
}
```

`target.protocol` may be `udp` or `tcp`.

- `udp`: node sends the decoded payload to the target UDP socket and returns
  the first upstream UDP response.
- `tcp`: node opens a TCP connection to the target, writes the decoded payload,
  half-closes the write side, and returns the first upstream response bytes.

Every target is checked against `route_policy.targets[]` when the session was
created with a dynamic route policy.

## Raw UDP Tunnel Frame

For real packet forwarding, the Windows capture layer should prefer the binary
raw UDP frame to avoid JSON/base64 overhead.

Request header:

```text
offset  size  field
0       4     magic = "XAU1"
4       1     version = 1
5       1     kind = 1
6       1     flags = 0
7       1     reserved = 0
8       2     session_id_len, big-endian
10      2     target_id_len, big-endian
12      2     host_len, big-endian
14      2     port, big-endian
16      4     payload_len, big-endian
20      N     session_id UTF-8
...     N     target_id UTF-8, optional
...     N     host UTF-8
...     N     raw UDP payload bytes
```

Response header:

```text
offset  size  field
0       4     magic = "XAU1"
4       1     version = 1
5       1     kind = 2
6       1     status_code: 0 forwarded, 1 upstream_timeout, 2 error
7       1     reserved = 0
8       2     session_id_len, big-endian
10      2     status_len, big-endian
12      2     reserved = 0
14      2     reserved = 0
16      4     payload_len, big-endian
20      N     session_id UTF-8
...     N     status UTF-8
...     N     raw upstream response bytes
```

Status values currently include:

- `forwarded`
- `upstream_timeout`
- `session_not_found`
- `session_expired`
- `relay_auth_required`
- `missing_target`
- `target_not_allowed`
- `target_protocol_unsupported`
- `relay_error`

## Windows Capture Layer Contract

The Windows capture layer should:

1. Request an `accel_ticket` from the business backend or control flow.
2. Send `probe` to the selected node over UDP, TCP, or QUIC.
3. Keep the returned `session_id`.
4. Observe game process traffic locally.
5. Match each outbound packet against `route_policy.targets[]` by protocol,
   domain or observed IP, CIDR, and port range.
6. Send matched UDP payloads as raw UDP tunnel frames.
7. Write returned raw response bytes back into the local game flow.
8. Renew the ticket before credential expiry and re-probe when the session
   expires.

The node does not load game catalogs, region catalogs, entitlement state, or
route tables. It trusts only the signed token and the token-bound route policy.
