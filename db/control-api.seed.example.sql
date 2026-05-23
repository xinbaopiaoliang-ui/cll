-- Example rows for local control-api testing.
-- Replace node_secret with the value in /var/lib/xaccel-node/bootstrap-response.json.

INSERT INTO accel_nodes (
  id,
  name,
  server_ip,
  server_port,
  bandwidth_quality,
  disable_quic,
  area,
  tag,
  status,
  node_secret,
  last_seen_at,
  kernel_version,
  config_revision
) VALUES (
  1,
  'standalone-103-201-131-99',
  '103.201.131.99',
  666,
  'fast',
  0,
  'UNKNOWN',
  'standalone',
  'online',
  'PASTE_NODE_SECRET',
  CURRENT_TIMESTAMP,
  '0.12.0',
  1
) ON DUPLICATE KEY UPDATE
  server_ip = VALUES(server_ip),
  server_port = VALUES(server_port),
  bandwidth_quality = VALUES(bandwidth_quality),
  disable_quic = VALUES(disable_quic),
  status = VALUES(status),
  node_secret = VALUES(node_secret),
  last_seen_at = VALUES(last_seen_at),
  kernel_version = VALUES(kernel_version);

INSERT INTO game_route_rules (
  game_id,
  node_id,
  target_addr,
  protocol,
  area,
  tag,
  priority,
  status
) VALUES (
  8888,
  1,
  '127.0.0.1:7777',
  'udp',
  'UNKNOWN',
  'standalone',
  10,
  'enabled'
) ON DUPLICATE KEY UPDATE
  target_addr = VALUES(target_addr),
  priority = VALUES(priority),
  status = VALUES(status);
