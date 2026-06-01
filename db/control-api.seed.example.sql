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
  '0.31.0',
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

INSERT INTO accel_games (
  game_id,
  name,
  platform,
  category,
  status,
  remark
) VALUES (
  8888,
  'Local Echo Test',
  'pc',
  'test',
  'enabled',
  'Local UDP echo route for control-plane validation'
) ON DUPLICATE KEY UPDATE
  name = VALUES(name),
  platform = VALUES(platform),
  category = VALUES(category),
  status = VALUES(status),
  remark = VALUES(remark);

INSERT INTO accel_game_regions (
  game_id,
  region_id,
  name,
  area,
  status,
  remark
) VALUES (
  8888,
  1,
  'Default Region',
  'UNKNOWN',
  'enabled',
  'Default test region for local UDP echo validation'
) ON DUPLICATE KEY UPDATE
  name = VALUES(name),
  area = VALUES(area),
  status = VALUES(status),
  remark = VALUES(remark);

INSERT INTO game_route_rules (
  game_id,
  game_name,
  region_id,
  region_name,
  node_id,
  target_addr,
  protocol,
  area,
  tag,
  priority,
  status,
  sync_source,
  external_id
) VALUES (
  8888,
  'Local Echo Test',
  NULL,
  NULL,
  1,
  '127.0.0.1:7777',
  'udp',
  'UNKNOWN',
  'standalone',
  10,
  'enabled',
  'seed',
  'local-echo-default'
) ON DUPLICATE KEY UPDATE
  game_name = VALUES(game_name),
  region_id = VALUES(region_id),
  region_name = VALUES(region_name),
  target_addr = VALUES(target_addr),
  priority = VALUES(priority),
  status = VALUES(status),
  sync_source = VALUES(sync_source),
  external_id = VALUES(external_id);
