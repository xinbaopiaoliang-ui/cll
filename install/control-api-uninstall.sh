#!/usr/bin/env bash
set -Eeuo pipefail

UNINSTALLER_VERSION="0.50.0"
SERVICE_NAME="xaccel-control-api"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/xaccel-control-api"
LOG_DIR="/var/log/xaccel-control-api"
SYSTEMD_UNIT="/etc/systemd/system/${SERVICE_NAME}.service"
ENV_FILE="${CONFIG_DIR}/control-api.env"

PURGE="0"
PURGE_DB="0"
DB_NAME="xaccel"
DB_USER="xaccel"
MYSQL_ROOT_PASSWORD=""
MYSQL_ROOT_CNF="/root/.xaccel-mysql-root.cnf"
MYSQL_DROP_HOSTS=("localhost" "127.0.0.1" "172.17.0.1" "172.17.%")

usage() {
  cat <<'USAGE'
Usage:
  control-api-uninstall.sh [options]

Options:
  --purge                  Remove config and logs.
  --purge-db               Drop xaccel database and DB users. Use only after backup.
  --db-name NAME           Database name to drop with --purge-db. Default: xaccel.
  --db-user USER           Database user to drop with --purge-db. Default: xaccel.
  --mysql-root-password P  MySQL root password for --purge-db.
  --mysql-root-cnf PATH    MySQL root defaults file. Default: /root/.xaccel-mysql-root.cnf.
  --mysql-drop-host HOST   Extra DB user host to drop. Can be repeated.
  -h, --help               Show help.

Examples:
  # Remove service only; keep config, logs and database.
  control-api-uninstall.sh

  # Remove service, config and logs; keep database.
  control-api-uninstall.sh --purge

  # Full cleanup after backup.
  control-api-uninstall.sh --purge --purge-db --mysql-root-password 'MysqlRoot_2026'
USAGE
}

log() {
  printf '[xaccel-control-uninstall] %s\n' "$*"
}

fail() {
  printf '[xaccel-control-uninstall] ERROR: %s\n' "$*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --purge)
      PURGE="1"
      shift
      ;;
    --purge-db)
      PURGE_DB="1"
      shift
      ;;
    --db-name)
      DB_NAME="${2:-}"
      shift 2
      ;;
    --db-user)
      DB_USER="${2:-}"
      shift 2
      ;;
    --mysql-root-password)
      MYSQL_ROOT_PASSWORD="${2:-}"
      shift 2
      ;;
    --mysql-root-cnf)
      MYSQL_ROOT_CNF="${2:-}"
      shift 2
      ;;
    --mysql-drop-host)
      MYSQL_DROP_HOSTS+=("${2:-}")
      shift 2
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

mysql_root_cmd() {
  if [[ -n "$MYSQL_ROOT_PASSWORD" ]]; then
    mysql -uroot -p"${MYSQL_ROOT_PASSWORD}" "$@"
  elif [[ -f "$MYSQL_ROOT_CNF" ]]; then
    mysql --defaults-extra-file="$MYSQL_ROOT_CNF" "$@"
  else
    mysql -uroot "$@"
  fi
}

purge_database_if_requested() {
  [[ "$PURGE_DB" == "1" ]] || return 0
  command -v mysql >/dev/null 2>&1 || fail "mysql client is required for --purge-db"
  validate_identifier "$DB_NAME" "--db-name"
  validate_identifier "$DB_USER" "--db-user"

  log "drop database=${DB_NAME} and user=${DB_USER} hosts=${MYSQL_DROP_HOSTS[*]}"
  local db_name_sql db_user_sql host host_sql
  db_name_sql="\`${DB_NAME}\`"
  db_user_sql="$(sql_string "$DB_USER")"
  {
    printf 'DROP DATABASE IF EXISTS %s;\n' "$db_name_sql"
    for host in "${MYSQL_DROP_HOSTS[@]}"; do
      [[ -n "$host" ]] || continue
      host_sql="$(sql_string "$host")"
      printf 'DROP USER IF EXISTS %s@%s;\n' "$db_user_sql" "$host_sql"
    done
    printf 'FLUSH PRIVILEGES;\n'
  } | mysql_root_cmd
}

main() {
  [[ "$(id -u)" == "0" ]] || fail "please run as root"
  log "uninstaller=${UNINSTALLER_VERSION}"

  if command -v systemctl >/dev/null 2>&1; then
    systemctl stop "$SERVICE_NAME" >/dev/null 2>&1 || true
    systemctl disable "$SERVICE_NAME" >/dev/null 2>&1 || true
  fi

  rm -f "${INSTALL_DIR}/${SERVICE_NAME}"
  rm -f "$SYSTEMD_UNIT"

  if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload
  fi

  if [[ "$PURGE" == "1" ]]; then
    rm -rf "$CONFIG_DIR" "$LOG_DIR"
    log "removed config and logs"
  else
    log "removed service; kept ${ENV_FILE}, ${CONFIG_DIR} and ${LOG_DIR}"
  fi

  purge_database_if_requested
  log "uninstalled"
}

main "$@"
