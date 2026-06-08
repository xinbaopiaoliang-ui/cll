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

CREATE TABLE accel_games (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  game_id BIGINT UNSIGNED NOT NULL,
  name VARCHAR(128) NOT NULL,
  platform ENUM('pc', 'android', 'ios', 'multi') NOT NULL DEFAULT 'pc',
  category VARCHAR(64) NULL,
  icon_url VARCHAR(512) NULL,
  status ENUM('enabled', 'disabled') NOT NULL DEFAULT 'enabled',
  remark VARCHAR(512) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_game_id (game_id),
  INDEX idx_status_platform (status, platform),
  INDEX idx_category (category)
);

CREATE TABLE accel_game_regions (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  game_id BIGINT UNSIGNED NOT NULL,
  region_id BIGINT UNSIGNED NOT NULL,
  name VARCHAR(128) NOT NULL,
  area VARCHAR(32) NULL,
  status ENUM('enabled', 'disabled') NOT NULL DEFAULT 'enabled',
  remark VARCHAR(512) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uniq_game_region (game_id, region_id),
  INDEX idx_game_status (game_id, status),
  INDEX idx_area (area)
);

CREATE TABLE game_route_rules (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  game_id BIGINT UNSIGNED NOT NULL,
  game_name VARCHAR(128) NOT NULL DEFAULT '',
  region_id BIGINT UNSIGNED NULL,
  region_name VARCHAR(128) NULL,
  node_id BIGINT UNSIGNED NOT NULL,
  target_addr VARCHAR(255) NOT NULL,
  protocol ENUM('udp') NOT NULL DEFAULT 'udp',
  area VARCHAR(32) NULL,
  tag VARCHAR(64) NULL,
  priority INT UNSIGNED NOT NULL DEFAULT 100,
  status ENUM('enabled', 'disabled') NOT NULL DEFAULT 'enabled',
  sync_source VARCHAR(32) NULL,
  external_id VARCHAR(128) NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_game_status_priority (game_id, status, priority),
  INDEX idx_game_region_status_priority (game_id, region_id, status, priority),
  INDEX idx_game_node_region_target (game_id, region_id, node_id, target_addr, protocol),
  INDEX idx_node_id (node_id),
  UNIQUE KEY idx_route_external (sync_source, external_id),
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
  region_id BIGINT UNSIGNED NULL,
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
  INDEX idx_game_region_created (game_id, region_id, created_at),
  INDEX idx_node_created (node_id, created_at),
  INDEX idx_expires_at (expires_at),
  CONSTRAINT fk_intent_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE admin_users (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  username VARCHAR(64) NOT NULL,
  display_name VARCHAR(128) NULL,
  password_hash VARCHAR(255) NOT NULL,
  role ENUM('super_admin', 'operator', 'viewer') NOT NULL DEFAULT 'viewer',
  status ENUM('active', 'disabled') NOT NULL DEFAULT 'active',
  last_login_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uk_admin_username (username),
  INDEX idx_role_status (role, status)
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

CREATE TABLE node_remote_tasks (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  task_type VARCHAR(32) NOT NULL,
  status ENUM('pending', 'running', 'succeeded', 'failed', 'canceled') NOT NULL DEFAULT 'pending',
  message VARCHAR(512) NULL,
  output TEXT NULL,
  error_message VARCHAR(512) NULL,
  requested_by VARCHAR(64) NULL,
  claimed_at TIMESTAMP NULL,
  started_at TIMESTAMP NULL,
  finished_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_node_status_created (node_id, status, created_at),
  INDEX idx_status_created (status, created_at),
  CONSTRAINT fk_remote_task_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_ssh_credentials (
  node_id BIGINT UNSIGNED PRIMARY KEY,
  host VARCHAR(128) NOT NULL,
  port INT UNSIGNED NOT NULL DEFAULT 22,
  username VARCHAR(64) NOT NULL,
  password_ciphertext TEXT NOT NULL,
  password_nonce VARCHAR(64) NOT NULL,
  auth_status ENUM('untested', 'ok', 'failed') NOT NULL DEFAULT 'untested',
  last_error TEXT NULL,
  last_checked_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  CONSTRAINT fk_ssh_credential_node
    FOREIGN KEY (node_id) REFERENCES accel_nodes(id)
    ON DELETE CASCADE
);

CREATE TABLE node_operation_tasks (
  id BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT,
  node_id BIGINT UNSIGNED NOT NULL,
  action VARCHAR(64) NOT NULL,
  executor VARCHAR(32) NOT NULL DEFAULT 'control_ssh',
  status ENUM('running', 'succeeded', 'failed') NOT NULL DEFAULT 'running',
  command_label VARCHAR(128) NOT NULL,
  exit_code INT NULL,
  duration_ms BIGINT UNSIGNED NULL,
  output MEDIUMTEXT NULL,
  error_message TEXT NULL,
  version_check_json JSON NULL,
  started_at TIMESTAMP NULL,
  finished_at TIMESTAMP NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  INDEX idx_node_created (node_id, created_at),
  INDEX idx_status_created (status, created_at),
  INDEX idx_action_created (action, created_at),
  CONSTRAINT fk_operation_task_node
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
