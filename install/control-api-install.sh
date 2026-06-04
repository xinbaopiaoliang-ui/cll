#!/usr/bin/env bash
set -Eeuo pipefail

INSTALLER_VERSION="0.39.3"
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
CREDENTIAL_KEY=""
ARTIFACT_URL=""
SHA256_URL=""
DRY_RUN="0"

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
                         Optional bearer token for business backend catalog sync API.
  --credential-key KEY   Base64 32-byte key for encrypting saved SSH passwords.
                         Generated automatically when omitted.
  --artifact-url URL      Override xaccel-control-api tar.gz download URL.
  --sha256-url URL        Override xaccel-control-api sha256 download URL.
  --dry-run               Run preflight only and print planned actions.
  -h, --help              Show this help.
USAGE
}

log() {
  printf '[xaccel-control-installer] %s\n' "$*"
}

fail() {
  printf '[xaccel-control-installer] ERROR: %s\n' "$*" >&2
  exit 1
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
    --dry-run)
      DRY_RUN="1"
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

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    aarch64|arm64) echo "aarch64" ;;
    *) fail "unsupported arch: $(uname -m)" ;;
  esac
}

preflight() {
  require_root
  command -v systemctl >/dev/null 2>&1 || fail "systemd is required"
  command -v curl >/dev/null 2>&1 || fail "curl is required"
  command -v tar >/dev/null 2>&1 || fail "tar is required"

  [[ -n "$DATABASE_URL" ]] || fail "--database-url is required"
  [[ -n "$LISTEN" ]] || fail "--listen is required"
  [[ "$TOKEN_TTL_SEC" =~ ^[0-9]+$ ]] || fail "--token-ttl-sec must be numeric"
  [[ "$MAX_DB_CONNECTIONS" =~ ^[0-9]+$ ]] || fail "--max-db-connections must be numeric"
  (( TOKEN_TTL_SEC >= 1 )) || fail "--token-ttl-sec must be positive"
  (( MAX_DB_CONNECTIONS >= 1 )) || fail "--max-db-connections must be positive"
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
  curl -fsSL "$artifact_url" -o "$tar_file" || {
    rm -rf "$tmp_dir"
    fail "failed to download release artifact"
  }

  log "download checksum: ${sha_url}"
  curl -fsSL "$sha_url" -o "$sha_file" || {
    rm -rf "$tmp_dir"
    fail "failed to download sha256 file"
  }

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

  install_binary_release
  install_ssh_tools_if_possible
  generate_admin_token_if_needed
  load_existing_business_sync_token_if_needed
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
