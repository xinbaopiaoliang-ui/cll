#!/usr/bin/env bash
set -Eeuo pipefail

INSTALLER_VERSION="0.70.1"
SERVICE_NAME="xaccel-control-api"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/xaccel-control-api"
LOG_DIR="/var/log/xaccel-control-api"
ENV_FILE="${CONFIG_DIR}/control-api.env"
SYSTEMD_UNIT="/etc/systemd/system/${SERVICE_NAME}.service"
GITHUB_REPO="xinbaopiaoliang-ui/cll"
RELEASE_BASE_URL="https://github.com/${GITHUB_REPO}/releases/latest/download"

DATABASE_URL=""
LISTEN="127.0.0.1:18080"
TOKEN_TTL_SEC="120"
MAX_DB_CONNECTIONS="8"
ADMIN_TOKEN=""
PUBLIC_BASE_URL=""
BUSINESS_SYNC_TOKEN=""
CLIENT_API_TOKEN=""
CREDENTIAL_KEY=""
ARTIFACT_URL=""
SHA256_URL=""
DOWNLOAD_MODE="github-api-first"
DRY_RUN="0"
INIT_MYSQL="0"
MYSQL_ROOT_PASSWORD=""
MYSQL_ROOT_CNF="/root/.xaccel-mysql-root.cnf"
MYSQL_GRANT_HOSTS=("localhost" "127.0.0.1" "172.17.0.1" "172.17.%")
SKIP_DB_PREFLIGHT="0"

DB_USER=""
DB_PASSWORD=""
DB_HOST=""
DB_PORT="3306"
DB_NAME=""

usage() {
  cat <<'USAGE'
Usage:
  control-api-install.sh --database-url URL [options]

Options:
  --database-url URL       MySQL URL, for example mysql://xaccel:password@127.0.0.1:3306/xaccel.
  --listen ADDR           HTTP listen address. Default: 127.0.0.1:18080.
  --token-ttl-sec SEC     Client token TTL. Default: 120.
  --max-db-connections N  MySQL connection pool size. Default: 8.
  --admin-token TOKEN     Admin API bearer token. Generated automatically when omitted.
  --public-base-url URL   Optional public base URL for node bootstrap responses.
  --business-sync-token TOKEN
                         Optional bearer token for business backend API.
  --client-api-token TOKEN
                         Optional token required by /api/client/v1/connect-intent.
  --credential-key KEY   Base64 32-byte key for encrypting saved SSH passwords.
                         Generated automatically when omitted.
  --artifact-url URL      Override xaccel-control-api tar.gz download URL.
  --sha256-url URL        Override xaccel-control-api sha256 download URL.
  --download-mode MODE    github-api-first or standard-first. Default: github-api-first.
  --dry-run               Run preflight only and print planned actions.
  --init-mysql            Create database/user and grant local Docker bridge hosts before install.
  --mysql-root-password P MySQL root password for --init-mysql.
  --mysql-root-cnf PATH   MySQL root defaults file. Default: /root/.xaccel-mysql-root.cnf.
  --mysql-grant-host HOST Extra app user grant host. Can be repeated.
  --skip-db-preflight     Skip mysql client connection check.
  -h, --help              Show this help.

Examples:
  # Existing database/user already works.
  control-api-install.sh \
    --database-url 'mysql://xaccel:xaccel_password@127.0.0.1:3306/xaccel' \
    --listen 0.0.0.0:18080 \
    --public-base-url http://103.201.131.99:18080

  # New control-panel server: create DB/user and grant 127.0.0.1 + 172.17.*.
  control-api-install.sh \
    --database-url 'mysql://xaccel:xaccel_password@127.0.0.1:3306/xaccel' \
    --init-mysql \
    --mysql-root-password 'MysqlRoot_2026' \
    --listen 0.0.0.0:18080 \
    --public-base-url http://103.201.131.99:18080
USAGE
}

log() {
  printf '[xaccel-control-installer] %s\n' "$*"
}

fail() {
  printf '[xaccel-control-installer] ERROR: %s\n' "$*" >&2
  exit 1
}

download_file() {
  local url output
  url="$1"
  output="$2"
  curl -fsSL \
    --retry 5 \
    --retry-delay 3 \
    --connect-timeout 20 \
    --max-time 300 \
    "$url" \
    -o "$output"
}

github_latest_asset_id() {
  local asset_name
  asset_name="$1"
  curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" |
    awk -v target="$asset_name" '
      /"id":/ {
        id = $0
        gsub(/[^0-9]/, "", id)
      }
      index($0, "\"name\": \"" target "\"") {
        print id
        exit
      }
    '
}

download_latest_github_asset() {
  local asset_name output asset_id
  asset_name="$1"
  output="$2"
  asset_id="$(github_latest_asset_id "$asset_name")"
  [[ -n "$asset_id" ]] || return 1
  curl -fsSL \
    -H "Accept: application/octet-stream" \
    --retry 5 \
    --retry-delay 3 \
    --connect-timeout 20 \
    --max-time 300 \
    "https://api.github.com/repos/${GITHUB_REPO}/releases/assets/${asset_id}" \
    -o "$output"
}

download_default_release_asset() {
  local asset_name url output
  asset_name="$1"
  url="$2"
  output="$3"
  if [[ "$DOWNLOAD_MODE" == "standard-first" ]]; then
    download_file "$url" "$output" || download_latest_github_asset "$asset_name" "$output"
  else
    download_latest_github_asset "$asset_name" "$output" || download_file "$url" "$output"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --database-url)
      DATABASE_URL="${2:-}"
      shift 2
      ;;
    --listen)
      LISTEN="${2:-}"
      shift 2
      ;;
    --token-ttl-sec)
      TOKEN_TTL_SEC="${2:-}"
      shift 2
      ;;
    --max-db-connections)
      MAX_DB_CONNECTIONS="${2:-}"
      shift 2
      ;;
    --admin-token)
      ADMIN_TOKEN="${2:-}"
      shift 2
      ;;
    --public-base-url)
      PUBLIC_BASE_URL="${2:-}"
      shift 2
      ;;
    --business-sync-token)
      BUSINESS_SYNC_TOKEN="${2:-}"
      shift 2
      ;;
    --client-api-token)
      CLIENT_API_TOKEN="${2:-}"
      shift 2
      ;;
    --credential-key)
      CREDENTIAL_KEY="${2:-}"
      shift 2
      ;;
    --artifact-url)
      ARTIFACT_URL="${2:-}"
      shift 2
      ;;
    --sha256-url)
      SHA256_URL="${2:-}"
      shift 2
      ;;
    --download-mode)
      DOWNLOAD_MODE="${2:-}"
      shift 2
      ;;
    --dry-run)
      DRY_RUN="1"
      shift
      ;;
    --init-mysql)
      INIT_MYSQL="1"
      shift
      ;;
    --mysql-root-password)
      MYSQL_ROOT_PASSWORD="${2:-}"
      shift 2
      ;;
    --mysql-root-cnf)
      MYSQL_ROOT_CNF="${2:-}"
      shift 2
      ;;
    --mysql-grant-host)
      MYSQL_GRANT_HOSTS+=("${2:-}")
      shift 2
      ;;
    --skip-db-preflight)
      SKIP_DB_PREFLIGHT="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

require_root() {
  [[ "$(id -u)" == "0" ]] || fail "please run as root"
}

validate_identifier() {
  local value="$1" field="$2"
  [[ -n "$value" ]] || fail "${field} is required"
  [[ "$value" =~ ^[A-Za-z0-9_]+$ ]] || fail "${field} may only contain letters, numbers, and underscore"
}

sql_string() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\'/\\\'}"
  printf "'%s'" "$value"
}

parse_database_url() {
  [[ "$DATABASE_URL" == mysql://* ]] || fail "--database-url must start with mysql://"

  local rest credentials host_path host_port
  rest="${DATABASE_URL#mysql://}"
  [[ "$rest" == *@* ]] || fail "--database-url must include user:password@host"

  credentials="${rest%%@*}"
  host_path="${rest#*@}"
  [[ "$credentials" == *:* ]] || fail "--database-url must include database password"

  DB_USER="${credentials%%:*}"
  DB_PASSWORD="${credentials#*:}"
  host_port="${host_path%%/*}"
  DB_NAME="${host_path#*/}"
  DB_NAME="${DB_NAME%%\?*}"

  if [[ "$host_port" == *:* ]]; then
    DB_HOST="${host_port%%:*}"
    DB_PORT="${host_port##*:}"
  else
    DB_HOST="$host_port"
    DB_PORT="3306"
  fi

  validate_identifier "$DB_NAME" "database name from --database-url"
  validate_identifier "$DB_USER" "database user from --database-url"
  [[ -n "$DB_PASSWORD" ]] || fail "database password from --database-url is empty"
  [[ -n "$DB_HOST" ]] || fail "database host from --database-url is empty"
  [[ "$DB_PORT" =~ ^[0-9]+$ ]] || fail "database port from --database-url must be numeric"
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    aarch64|arm64) echo "aarch64" ;;
    *) fail "unsupported arch: $(uname -m)" ;;
  esac
}

preflight() {
  require_root
  parse_database_url
  command -v systemctl >/dev/null 2>&1 || fail "systemd is required"
  command -v curl >/dev/null 2>&1 || fail "curl is required"
  command -v tar >/dev/null 2>&1 || fail "tar is required"

  [[ -n "$DATABASE_URL" ]] || fail "--database-url is required"
  [[ -n "$LISTEN" ]] || fail "--listen is required"
  [[ "$TOKEN_TTL_SEC" =~ ^[0-9]+$ ]] || fail "--token-ttl-sec must be numeric"
  [[ "$MAX_DB_CONNECTIONS" =~ ^[0-9]+$ ]] || fail "--max-db-connections must be numeric"
  (( TOKEN_TTL_SEC >= 1 )) || fail "--token-ttl-sec must be positive"
  (( MAX_DB_CONNECTIONS >= 1 )) || fail "--max-db-connections must be positive"
  case "$DOWNLOAD_MODE" in
    github-api-first|standard-first) ;;
    *) fail "--download-mode must be github-api-first or standard-first" ;;
  esac
  if [[ "$INIT_MYSQL" == "1" || "$SKIP_DB_PREFLIGHT" != "1" ]]; then
    command -v mysql >/dev/null 2>&1 || fail "mysql client is required; install mysql-client or rerun with --skip-db-preflight"
  fi
}

mysql_root_cmd() {
  if [[ -n "$MYSQL_ROOT_PASSWORD" ]]; then
    mysql -uroot -p"${MYSQL_ROOT_PASSWORD}" "$@"
  elif [[ -f "$MYSQL_ROOT_CNF" ]]; then
    mysql --defaults-extra-file="$MYSQL_ROOT_CNF" "$@"
  else
    mysql -uroot "$@"
  fi
}

init_mysql_if_requested() {
  [[ "$INIT_MYSQL" == "1" ]] || return 0

  log "initialize MySQL database=${DB_NAME} user=${DB_USER}"
  local db_name_sql db_user_sql db_password_sql grant_host host_sql
  db_name_sql="\`${DB_NAME}\`"
  db_user_sql="$(sql_string "$DB_USER")"
  db_password_sql="$(sql_string "$DB_PASSWORD")"

  {
    printf 'CREATE DATABASE IF NOT EXISTS %s CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;\n' "$db_name_sql"
    for grant_host in "${MYSQL_GRANT_HOSTS[@]}"; do
      [[ -n "$grant_host" ]] || continue
      host_sql="$(sql_string "$grant_host")"
      printf 'CREATE USER IF NOT EXISTS %s@%s IDENTIFIED BY %s;\n' "$db_user_sql" "$host_sql" "$db_password_sql"
      printf 'ALTER USER %s@%s IDENTIFIED BY %s;\n' "$db_user_sql" "$host_sql" "$db_password_sql"
      printf 'GRANT ALL PRIVILEGES ON %s.* TO %s@%s;\n' "$db_name_sql" "$db_user_sql" "$host_sql"
    done
    printf 'FLUSH PRIVILEGES;\n'
  } | mysql_root_cmd
}

test_database_connection() {
  [[ "$SKIP_DB_PREFLIGHT" == "1" ]] && return 0
  log "check MySQL connection: ${DB_USER}@${DB_HOST}:${DB_PORT}/${DB_NAME}"
  MYSQL_PWD="$DB_PASSWORD" mysql -h"$DB_HOST" -P"$DB_PORT" -u"$DB_USER" "$DB_NAME" -e "SELECT 1;" >/dev/null
}

generate_admin_token_if_needed() {
  if [[ -n "$ADMIN_TOKEN" ]]; then
    return 0
  fi

  if [[ -f "$ENV_FILE" ]]; then
    local existing_token
    existing_token="$(sed -n "s/^XACCEL_ADMIN_TOKEN='\(.*\)'$/\1/p" "$ENV_FILE" | tail -n 1 || true)"
    if [[ -n "$existing_token" ]]; then
      ADMIN_TOKEN="$existing_token"
      log "reuse existing admin token from ${ENV_FILE}"
      return 0
    fi
  fi

  if command -v openssl >/dev/null 2>&1; then
    ADMIN_TOKEN="$(openssl rand -base64 32)"
  else
    ADMIN_TOKEN="admin-$(date +%s)-$(hostname)"
  fi
}

load_existing_business_sync_token_if_needed() {
  if [[ -n "$BUSINESS_SYNC_TOKEN" ]]; then
    return 0
  fi

  if [[ -f "$ENV_FILE" ]]; then
    local existing_token
    existing_token="$(sed -n "s/^XACCEL_BUSINESS_SYNC_TOKEN='\(.*\)'$/\1/p" "$ENV_FILE" | tail -n 1 || true)"
    if [[ -n "$existing_token" ]]; then
      BUSINESS_SYNC_TOKEN="$existing_token"
      log "reuse existing business sync token from ${ENV_FILE}"
    fi
  fi
}

load_existing_client_api_token_if_needed() {
  if [[ -n "$CLIENT_API_TOKEN" ]]; then
    return 0
  fi

  if [[ -f "$ENV_FILE" ]]; then
    local existing_token
    existing_token="$(sed -n "s/^XACCEL_CLIENT_API_TOKEN='\(.*\)'$/\1/p" "$ENV_FILE" | tail -n 1 || true)"
    if [[ -n "$existing_token" ]]; then
      CLIENT_API_TOKEN="$existing_token"
      log "reuse existing client API token from ${ENV_FILE}"
    fi
  fi
}

generate_credential_key_if_needed() {
  if [[ -n "$CREDENTIAL_KEY" ]]; then
    return 0
  fi

  if [[ -f "$ENV_FILE" ]]; then
    local existing_key
    existing_key="$(sed -n "s/^XACCEL_CREDENTIAL_KEY='\(.*\)'$/\1/p" "$ENV_FILE" | tail -n 1 || true)"
    if [[ -n "$existing_key" ]]; then
      CREDENTIAL_KEY="$existing_key"
      log "reuse existing credential key from ${ENV_FILE}"
      return 0
    fi
  fi

  if command -v openssl >/dev/null 2>&1; then
    CREDENTIAL_KEY="$(openssl rand -base64 32)"
  else
    fail "openssl is required to generate XACCEL_CREDENTIAL_KEY"
  fi
}

install_ssh_tools_if_possible() {
  if command -v ssh >/dev/null 2>&1 && command -v sshpass >/dev/null 2>&1; then
    return 0
  fi

  if command -v apt-get >/dev/null 2>&1; then
    log "install ssh client tools for server account control"
    apt-get update >/dev/null 2>&1 || {
      log "warning: apt-get update failed; SSH account control may require manual sshpass install"
      return 0
    }
    DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-client sshpass >/dev/null 2>&1 || {
      log "warning: failed to install openssh-client/sshpass; SSH account control may require manual install"
      return 0
    }
    return 0
  fi

  log "warning: sshpass not installed and no supported package manager found; SSH account control may require manual install"
}

env_escape() {
  printf '%s' "$1" | sed "s/'/'\\\\''/g"
}

install_binary_release() {
  local arch artifact_name artifact_url sha_url tmp_dir tar_file sha_file extracted_bin
  arch="$(detect_arch)"
  artifact_name="xaccel-control-api-linux-${arch}.tar.gz"
  artifact_url="${ARTIFACT_URL:-${RELEASE_BASE_URL}/${artifact_name}}"
  sha_url="${SHA256_URL:-${artifact_url}.sha256}"
  tmp_dir="$(mktemp -d)"
  tar_file="${tmp_dir}/${artifact_name}"
  sha_file="${tar_file}.sha256"

  log "download control-api release: ${artifact_url}"
  if [[ -z "$ARTIFACT_URL" ]]; then
    if download_default_release_asset "$artifact_name" "$artifact_url" "$tar_file"; then
      log "downloaded control-api release with ${DOWNLOAD_MODE}"
    else
      rm -rf "$tmp_dir"
      fail "failed to download release artifact after retries. Check GitHub Release assets or server access to github.com/api.github.com"
    fi
  elif ! download_file "$artifact_url" "$tar_file"; then
    rm -rf "$tmp_dir"
    fail "failed to download release artifact after retries. Check artifact URL or server network"
  fi

  log "download checksum: ${sha_url}"
  if [[ -z "$SHA256_URL" ]]; then
    if download_default_release_asset "${artifact_name}.sha256" "$sha_url" "$sha_file"; then
      log "downloaded checksum with ${DOWNLOAD_MODE}"
    else
      rm -rf "$tmp_dir"
      fail "failed to download sha256 file after retries"
    fi
  elif ! download_file "$sha_url" "$sha_file"; then
    rm -rf "$tmp_dir"
    fail "failed to download sha256 file after retries"
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp_dir" && sha256sum -c "$(basename "$sha_file")")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$tmp_dir" && shasum -a 256 -c "$(basename "$sha_file")")
  else
    rm -rf "$tmp_dir"
    fail "sha256sum or shasum is required"
  fi

  tar -xzf "$tar_file" -C "$tmp_dir"
  extracted_bin="$(find "$tmp_dir" -type f -name "$SERVICE_NAME" | head -n 1)"
  [[ -n "$extracted_bin" ]] || {
    rm -rf "$tmp_dir"
    fail "release artifact does not contain ${SERVICE_NAME}"
  }

  install -m 0755 "$extracted_bin" "${INSTALL_DIR}/${SERVICE_NAME}"
  rm -rf "$tmp_dir"
  log "installed ${INSTALL_DIR}/${SERVICE_NAME}"
}

write_env() {
  mkdir -p "$CONFIG_DIR" "$LOG_DIR"
  chmod 0755 "$CONFIG_DIR" "$LOG_DIR"
  cat > "$ENV_FILE" <<EOF
DATABASE_URL='$(env_escape "$DATABASE_URL")'
XACCEL_CONTROL_LISTEN='$(env_escape "$LISTEN")'
XACCEL_TOKEN_TTL_SEC='$(env_escape "$TOKEN_TTL_SEC")'
XACCEL_MAX_DB_CONNECTIONS='$(env_escape "$MAX_DB_CONNECTIONS")'
XACCEL_ADMIN_TOKEN='$(env_escape "$ADMIN_TOKEN")'
XACCEL_CREDENTIAL_KEY='$(env_escape "$CREDENTIAL_KEY")'
RUST_LOG='xaccel_control_api=info'
EOF
  if [[ -n "$PUBLIC_BASE_URL" ]]; then
    printf "XACCEL_PUBLIC_BASE_URL='%s'\n" "$(env_escape "$PUBLIC_BASE_URL")" >> "$ENV_FILE"
  fi
  if [[ -n "$BUSINESS_SYNC_TOKEN" ]]; then
    printf "XACCEL_BUSINESS_SYNC_TOKEN='%s'\n" "$(env_escape "$BUSINESS_SYNC_TOKEN")" >> "$ENV_FILE"
  fi
  if [[ -n "$CLIENT_API_TOKEN" ]]; then
    printf "XACCEL_CLIENT_API_TOKEN='%s'\n" "$(env_escape "$CLIENT_API_TOKEN")" >> "$ENV_FILE"
  fi
  chmod 0600 "$ENV_FILE"
}

write_systemd_unit() {
  cat > "$SYSTEMD_UNIT" <<EOF
[Unit]
Description=XAccel Control API
After=network-online.target mysql.service mariadb.service
Wants=network-online.target

[Service]
Type=simple
User=root
Group=root
EnvironmentFile=${ENV_FILE}
ExecStart=${INSTALL_DIR}/${SERVICE_NAME} --listen \${XACCEL_CONTROL_LISTEN} --token-ttl-sec \${XACCEL_TOKEN_TTL_SEC} --max-db-connections \${XACCEL_MAX_DB_CONNECTIONS}
Restart=always
RestartSec=3
LimitNOFILE=65536
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF
}

enable_service() {
  systemctl daemon-reload
  systemctl enable "$SERVICE_NAME"
  systemctl restart "$SERVICE_NAME"
}

health_check() {
  local host_port host port health_url
  host_port="$LISTEN"
  host="${host_port%:*}"
  port="${host_port##*:}"
  if [[ "$host" == "0.0.0.0" || "$host" == "::" || "$host" == "[::]" ]]; then
    host="127.0.0.1"
  fi
  health_url="http://${host}:${port}/health"

  log "service started"
  for _ in 1 2 3 4 5; do
    if curl -fsSL "$health_url" >/dev/null 2>&1; then
      log "health ok: ${health_url}"
      return 0
    fi
    sleep 1
  done
  log "health not ready yet: ${health_url}"
  systemctl --no-pager --full status "$SERVICE_NAME" || true
}

main() {
  preflight
  log "installer=${INSTALLER_VERSION} arch=$(detect_arch) listen=${LISTEN}"
  if [[ "$DRY_RUN" == "1" ]]; then
    log "dry-run passed"
    exit 0
  fi

  init_mysql_if_requested
  test_database_connection
  install_binary_release
  install_ssh_tools_if_possible
  generate_admin_token_if_needed
  load_existing_business_sync_token_if_needed
  load_existing_client_api_token_if_needed
  generate_credential_key_if_needed
  write_env
  write_systemd_unit
  enable_service
  health_check

  log "installed"
  log "service: systemctl status ${SERVICE_NAME}"
  log "logs: journalctl -u ${SERVICE_NAME} -f"
  log "env: ${ENV_FILE}"
  log "admin token is saved in ${ENV_FILE}"
}

main "$@"
