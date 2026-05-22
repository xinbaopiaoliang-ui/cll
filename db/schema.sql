-- Draft schema for the game accelerator backend.
-- Target database: MySQL 8.0+ or compatible.

CREATE TABLE accel_nodes (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  name VARCHAR(128) NOT NULL,
  server_ip VARCHAR(64) NOT NULL,
  server_port INT UNSIGNED NOT NULL,
  relay_server_ip VARCHAR(64) NULL,
  relay_server_port INT UNSIGNED NULL,
  is_support_ipv6 TINYINT(1) NOT NULL DEFAULT 0,
  bandwidth_quality ENUM('fast', 'normal', 'slow') NOT NULL DEFAULT 'normal',
  disable_quic TINYINT(1) NOT NULL DEFAULT 0,
  area VARCHAR(32) NOT NULL,
  is_local_ip TINYINT(1) NOT NULL DEFAULT 0,
  telecom_ip VARCHAR(64) NULL,
  mobile_ip VARCHAR(64) NULL,
  unicom_ip VARCHAR(64) NULL,
  tag VARCHAR(64) NULL,
  status ENUM(
    'pending_install',
    'installing',
    'online',
    'degraded',
    'draining',
    'offline',
    'install_failed',
    'disabled'
  ) NOT NULL DEFAULT 'pending_install',
  node_secret_hash VARCHAR(255) NULL,
  -- MVP: control-api needs the node secret to sign xat.v1 client tokens.
  -- Production should store this encrypted or fetch it from a secret manager.
  node_secret VARCHAR(255) NULL,
  installed_at TIMESTAMP NULL,
  last_seen_at TIMESTAMP NULL,
  last_report_at TIMESTAMP NULL,
  kernel_version VARCHAR(32) NULL,
  config_revision BIGINT UNSIGNED NOT NULL DEFAULT 0,
  install_error_code VARCHAR(64) NULL,
  install_error_message VARCHAR(512) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_status (status),
  INDEX idx_area_quality (area, bandwidth_quality),
  INDEX idx_tag (tag),
  UNIQUE KEY uniq_server_endpoint (server_ip, server_port)
);

CREATE TABLE game_route_rules (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  game_id BIGINT UNSIGNED NOT NULL,
  node_id BIGINT UNSIGNED NOT NULL,
  target_addr VARCHAR(255) NOT NULL,
  protocol ENUM('udp') NOT NULL DEFAULT 'udp',
  area VARCHAR(32) NULL,
  tag VARCHAR(64) NULL,
  priority INT UNSIGNED NOT NULL DEFAULT 100,
  status ENUM('enabled', 'disabled') NOT NULL DEFAULT 'enabled',
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_game_status_priority (game_id, status, priority),
  INDEX idx_node_id (node_id),
  UNIQUE KEY uniq_game_node_target (game_id, node_id, target_addr, protocol),
  CONSTRAINT fk_route_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE connect_intents (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  intent_id VARCHAR(96) NOT NULL,
  user_id BIGINT UNSIGNED NOT NULL,
  device_id VARCHAR(128) NOT NULL,
  game_id BIGINT UNSIGNED NOT NULL,
  node_id BIGINT UNSIGNED NOT NULL,
  target_addr VARCHAR(255) NOT NULL,
  protocol ENUM('udp') NOT NULL DEFAULT 'udp',
  client_ip VARCHAR(64) NULL,
  client_isp VARCHAR(64) NULL,
  platform VARCHAR(32) NULL,
  bandwidth_quality ENUM('fast', 'normal', 'slow') NOT NULL DEFAULT 'normal',
  expires_at TIMESTAMP NOT NULL,
  consumed_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_intent_id (intent_id),
  INDEX idx_user_created (user_id, created_at),
  INDEX idx_device_created (device_id, created_at),
  INDEX idx_game_created (game_id, created_at),
  INDEX idx_node_created (node_id, created_at),
  INDEX idx_expires_at (expires_at),
  CONSTRAINT fk_intent_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_bootstrap_tokens (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  token_hash VARCHAR(255) NOT NULL,
  expires_at TIMESTAMP NOT NULL,
  used_at TIMESTAMP NULL,
  used_by_ip VARCHAR(64) NULL,
  created_by BIGINT UNSIGNED NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_token_hash (token_hash),
  INDEX idx_node_id (node_id),
  INDEX idx_expires_at (expires_at),
  CONSTRAINT fk_bootstrap_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_config_revisions (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  revision BIGINT UNSIGNED NOT NULL,
  config_json JSON NOT NULL,
  signature VARCHAR(512) NULL,
  created_by BIGINT UNSIGNED NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_node_revision (node_id, revision),
  CONSTRAINT fk_config_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_runtime_reports (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  config_revision BIGINT UNSIGNED NOT NULL,
  status VARCHAR(32) NOT NULL,
  active_sessions INT UNSIGNED NOT NULL DEFAULT 0,
  udp_sessions INT UNSIGNED NOT NULL DEFAULT 0,
  tcp_sessions INT UNSIGNED NOT NULL DEFAULT 0,
  rtt_ms_p50 INT UNSIGNED NULL,
  rtt_ms_p95 INT UNSIGNED NULL,
  jitter_ms_p95 INT UNSIGNED NULL,
  packet_loss_ppm INT UNSIGNED NULL,
  cpu_percent DECIMAL(5,2) NULL,
  mem_used BIGINT UNSIGNED NULL,
  mem_total BIGINT UNSIGNED NULL,
  rx_bps BIGINT UNSIGNED NULL,
  tx_bps BIGINT UNSIGNED NULL,
  raw_json JSON NULL,
  reported_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  INDEX idx_node_reported (node_id, reported_at),
  CONSTRAINT fk_report_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_traffic_logs (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  user_id BIGINT UNSIGNED NOT NULL,
  device_id VARCHAR(128) NOT NULL,
  game_id BIGINT UNSIGNED NOT NULL,
  up_bytes BIGINT UNSIGNED NOT NULL DEFAULT 0,
  down_bytes BIGINT UNSIGNED NOT NULL DEFAULT 0,
  recorded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  INDEX idx_user_recorded (user_id, recorded_at),
  INDEX idx_node_recorded (node_id, recorded_at),
  INDEX idx_game_recorded (game_id, recorded_at),
  CONSTRAINT fk_traffic_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_audit_logs (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  actor_type VARCHAR(32) NOT NULL,
  actor_id BIGINT UNSIGNED NULL,
  action VARCHAR(64) NOT NULL,
  detail_json JSON NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  INDEX idx_node_created (node_id, created_at),
  INDEX idx_action (action),
  CONSTRAINT fk_audit_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);
