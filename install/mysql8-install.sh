#!/usr/bin/env bash
set -Eeuo pipefail

INSTALLER_VERSION="0.2.0"
DB_NAME="xaccel"
DB_USER="xaccel"
DB_PASSWORD=""
MYSQL_ROOT_PASSWORD=""
BIND_ADDRESS="127.0.0.1"
ALLOW_REMOTE_USER="0"
IMPORT_SQL=""
SKIP_SCHEMA="0"
SCHEMA_URL="https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/db/schema.sql"
ROOT_CNF="/root/.xaccel-mysql-root.cnf"
DRY_RUN="0"
MYSQL_ROOT_MODE=""
ROOT_CNF_WRITTEN="0"

usage() {
  cat <<'USAGE'
Usage:
  mysql8-install.sh --db-password PASSWORD [options]

Options:
  --db-name NAME              Database name. Default: xaccel.
  --db-user USER              Application database user. Default: xaccel.
  --db-password PASSWORD      Application database password. Required.
  --mysql-root-password PASS  MySQL root password. Generated when MySQL is installed by this script.
  --bind-address ADDR         MySQL bind address. Default: 127.0.0.1.
  --allow-remote-user         Also create DB user at '%' for remote control-api access.
                              The script always creates localhost, 127.0.0.1, 172.17.0.1 and 172.17.% users.
  --import-sql PATH           Import an existing SQL backup after creating DB and user.
  --skip-schema               Do not load db/schema.sql when --import-sql is not used.
  --schema-url URL            Schema SQL URL. Default: project db/schema.sql from main.
  --dry-run                   Run preflight only and print planned actions.
  -h, --help                  Show this help.

Examples:
  curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/mysql8-install.sh | sudo bash -s -- \
    --db-password xaccel_password

  curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/mysql8-install.sh | sudo bash -s -- \
    --db-password xaccel_password \
    --import-sql /tmp/xaccel.sql
USAGE
}

log() {
  printf '[xaccel-mysql8-installer] %s\n' "$*"
}

fail() {
  printf '[xaccel-mysql8-installer] ERROR: %s\n' "$*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --db-name)
      DB_NAME="${2:-}"
      shift 2
      ;;
    --db-user)
      DB_USER="${2:-}"
      shift 2
      ;;
    --db-password)
      DB_PASSWORD="${2:-}"
      shift 2
      ;;
    --mysql-root-password)
      MYSQL_ROOT_PASSWORD="${2:-}"
      shift 2
      ;;
    --bind-address)
      BIND_ADDRESS="${2:-}"
      shift 2
      ;;
    --allow-remote-user)
      ALLOW_REMOTE_USER="1"
      shift
      ;;
    --import-sql)
      IMPORT_SQL="${2:-}"
      shift 2
      ;;
    --skip-schema)
      SKIP_SCHEMA="1"
      shift
      ;;
    --schema-url)
      SCHEMA_URL="${2:-}"
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

validate_name() {
  local value="$1"
  local field="$2"
  [[ -n "$value" ]] || fail "${field} is required"
  [[ "$value" =~ ^[A-Za-z0-9_]+$ ]] || fail "${field} may only contain letters, numbers, and underscore"
}

sql_string() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\'/\\\'}"
  printf "'%s'" "$value"
}

random_secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
  else
    printf 'mysql-root-%s-%s' "$(date +%s)" "$(hostname)"
  fi
}

preflight() {
  [[ "${EUID}" -eq 0 ]] || fail "please run as root or through sudo"
  [[ -f /etc/os-release ]] || fail "unsupported OS: /etc/os-release not found"
  validate_name "$DB_NAME" "--db-name"
  validate_name "$DB_USER" "--db-user"
  [[ -n "$DB_PASSWORD" ]] || fail "--db-password is required"
  if [[ -n "$IMPORT_SQL" && ! -f "$IMPORT_SQL" ]]; then
    fail "--import-sql file not found: ${IMPORT_SQL}"
  fi
  command -v apt-get >/dev/null 2>&1 || fail "apt-get is required; this installer supports Debian/Ubuntu"
  command -v curl >/dev/null 2>&1 || fail "curl is required"
}

mysql_version_major() {
  if ! command -v mysql >/dev/null 2>&1; then
    return 1
  fi
  mysql --version | sed -n 's/.*Distrib \([0-9]\+\)\..*/\1/p' | head -n 1
}

mysql_is_installed() {
  local major
  major="$(mysql_version_major || true)"
  [[ "$major" == "8" ]]
}

generate_root_password_if_needed() {
  if [[ -z "$MYSQL_ROOT_PASSWORD" ]]; then
    MYSQL_ROOT_PASSWORD="$(random_secret)"
    log "generated MySQL root password; it will be saved to ${ROOT_CNF}"
  fi
}

write_root_cnf() {
  umask 077
  cat > "$ROOT_CNF" <<EOF
[client]
user=root
password=${MYSQL_ROOT_PASSWORD}
host=127.0.0.1
EOF
  chmod 0600 "$ROOT_CNF"
  ROOT_CNF_WRITTEN="1"
}

mysql_root_cmd() {
  if [[ "$MYSQL_ROOT_MODE" == "cnf" ]]; then
    mysql --defaults-extra-file="$ROOT_CNF" "$@"
  else
    mysql -uroot "$@"
  fi
}

ensure_root_access() {
  if [[ -n "$MYSQL_ROOT_PASSWORD" ]]; then
    write_root_cnf
    if mysql --defaults-extra-file="$ROOT_CNF" -e "SELECT 1;" >/dev/null 2>&1; then
      MYSQL_ROOT_MODE="cnf"
      return 0
    fi
  fi

  if [[ -f "$ROOT_CNF" ]] && mysql --defaults-extra-file="$ROOT_CNF" -e "SELECT 1;" >/dev/null 2>&1; then
    MYSQL_ROOT_MODE="cnf"
    return 0
  fi

  if mysql -uroot -e "SELECT 1;" >/dev/null 2>&1; then
    MYSQL_ROOT_MODE="socket"
    if [[ "$ROOT_CNF_WRITTEN" == "1" ]]; then
      rm -f "$ROOT_CNF"
    fi
    return 0
  fi

  fail "cannot login to MySQL as root; rerun with --mysql-root-password or fix local root access"
}

apt_update() {
  DEBIAN_FRONTEND=noninteractive apt-get update
}

install_base_packages() {
  DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl gnupg lsb-release debconf-utils
}

codename_supported_by_mysql_repo() {
  case "$1:$2" in
    debian:bullseye|debian:bookworm|debian:trixie)
      return 0
      ;;
    ubuntu:focal|ubuntu:jammy|ubuntu:noble)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

install_mysql_apt_repo() {
  # shellcheck disable=SC1091
  . /etc/os-release
  local family codename repo_base source_file keyring
  family=""
  if [[ "${ID:-}" == "debian" || "${ID_LIKE:-}" == *"debian"* ]]; then
    family="debian"
  fi
  if [[ "${ID:-}" == "ubuntu" || "${ID_LIKE:-}" == *"ubuntu"* ]]; then
    family="ubuntu"
  fi
  [[ -n "$family" ]] || fail "unsupported apt family: ID=${ID:-unknown} ID_LIKE=${ID_LIKE:-unknown}"
  codename="${VERSION_CODENAME:-}"
  [[ -n "$codename" ]] || codename="$(lsb_release -cs)"
  codename_supported_by_mysql_repo "$family" "$codename" || {
    fail "Oracle MySQL APT repo support is unknown for ${family}/${codename}; install MySQL 8 manually or use a supported Debian/Ubuntu release"
  }

  repo_base="http://repo.mysql.com/apt/${family}/"
  source_file="/etc/apt/sources.list.d/mysql-community.list"
  keyring="/usr/share/keyrings/mysql-community.gpg"

  log "configure Oracle MySQL APT repo: ${family}/${codename} mysql-8.0"
  curl -fsSL "https://repo.mysql.com/RPM-GPG-KEY-mysql-2023" | gpg --dearmor > "$keyring"
  chmod 0644 "$keyring"
  cat > "$source_file" <<EOF
deb [signed-by=${keyring}] ${repo_base} ${codename} mysql-8.0
EOF
}

preseed_mysql() {
  debconf-set-selections <<EOF
mysql-community-server mysql-community-server/root-pass password ${MYSQL_ROOT_PASSWORD}
mysql-community-server mysql-community-server/re-root-pass password ${MYSQL_ROOT_PASSWORD}
mysql-community-server mysql-server/default-auth-override select Use Strong Password Encryption (RECOMMENDED)
EOF
}

package_candidate_exists() {
  local package="$1"
  local candidate
  candidate="$(apt-cache policy "$package" | awk '/Candidate:/ {print $2; exit}')"
  [[ -n "$candidate" && "$candidate" != "(none)" ]]
}

install_mysql_server() {
  if mysql_is_installed; then
    log "MySQL 8 already installed: $(mysql --version)"
    return 0
  fi

  log "install base packages"
  apt_update
  install_base_packages

  if ! package_candidate_exists mysql-server; then
    install_mysql_apt_repo
    apt_update
  fi

  generate_root_password_if_needed
  preseed_mysql

  log "install MySQL 8 community server"
  if package_candidate_exists mysql-community-server; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y mysql-community-server
  else
    DEBIAN_FRONTEND=noninteractive apt-get install -y mysql-server
  fi

  systemctl enable mysql >/dev/null 2>&1 || true
  systemctl restart mysql

  mysql_is_installed || fail "mysql installed, but version is not MySQL 8: $(mysql --version || true)"
}

configure_bind_address() {
  local conf_dir conf_file
  if [[ -d /etc/mysql/mysql.conf.d ]]; then
    conf_dir="/etc/mysql/mysql.conf.d"
  else
    conf_dir="/etc/mysql/conf.d"
  fi
  conf_file="${conf_dir}/99-xaccel-bind.cnf"
  mkdir -p "$conf_dir"
  cat > "$conf_file" <<EOF
[mysqld]
bind-address=${BIND_ADDRESS}
EOF
  systemctl restart mysql
}

create_database_user() {
  local db_password_sql
  db_password_sql="$(sql_string "$DB_PASSWORD")"

  log "create database and local users: ${DB_NAME}/${DB_USER}@127.0.0.1,localhost,172.17.0.1,172.17.%"
  mysql_root_cmd <<SQL
CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE USER IF NOT EXISTS '${DB_USER}'@'localhost' IDENTIFIED BY ${db_password_sql};
ALTER USER '${DB_USER}'@'localhost' IDENTIFIED BY ${db_password_sql};
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'localhost';
CREATE USER IF NOT EXISTS '${DB_USER}'@'127.0.0.1' IDENTIFIED BY ${db_password_sql};
ALTER USER '${DB_USER}'@'127.0.0.1' IDENTIFIED BY ${db_password_sql};
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'127.0.0.1';
CREATE USER IF NOT EXISTS '${DB_USER}'@'172.17.0.1' IDENTIFIED BY ${db_password_sql};
ALTER USER '${DB_USER}'@'172.17.0.1' IDENTIFIED BY ${db_password_sql};
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'172.17.0.1';
CREATE USER IF NOT EXISTS '${DB_USER}'@'172.17.%' IDENTIFIED BY ${db_password_sql};
ALTER USER '${DB_USER}'@'172.17.%' IDENTIFIED BY ${db_password_sql};
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'172.17.%';
SQL

  if [[ "$ALLOW_REMOTE_USER" == "1" ]]; then
    log "create remote database user: ${DB_USER}@%"
    mysql_root_cmd <<SQL
CREATE USER IF NOT EXISTS '${DB_USER}'@'%' IDENTIFIED BY ${db_password_sql};
ALTER USER '${DB_USER}'@'%' IDENTIFIED BY ${db_password_sql};
GRANT ALL PRIVILEGES ON \`${DB_NAME}\`.* TO '${DB_USER}'@'%';
SQL
  fi

  mysql_root_cmd -e "FLUSH PRIVILEGES;"
}

import_database() {
  if [[ -n "$IMPORT_SQL" ]]; then
    log "import SQL backup: ${IMPORT_SQL}"
    mysql_root_cmd "$DB_NAME" < "$IMPORT_SQL"
    return 0
  fi

  if [[ "$SKIP_SCHEMA" == "1" ]]; then
    log "skip schema import"
    return 0
  fi

  local tmp_schema
  tmp_schema="$(mktemp)"
  log "download schema: ${SCHEMA_URL}"
  curl -fsSL "$SCHEMA_URL" -o "$tmp_schema"
  log "import schema into ${DB_NAME}"
  mysql_root_cmd "$DB_NAME" < "$tmp_schema"
  rm -f "$tmp_schema"
}

verify_app_user() {
  mysql -h127.0.0.1 -u"$DB_USER" -p"$DB_PASSWORD" "$DB_NAME" -e "SELECT VERSION() AS mysql_version, DATABASE() AS db_name;" >/dev/null
  log "application user verified"
}

print_summary() {
  log "installed"
  log "mysql version: $(mysql --version)"
  log "root credentials file: ${ROOT_CNF}"
  log "database url: mysql://${DB_USER}:<password>@127.0.0.1:3306/${DB_NAME}"
  log "control-api install example:"
  cat <<EOF
curl -fsSL https://raw.githubusercontent.com/xinbaopiaoliang-ui/cll/main/install/control-api-install.sh | sudo bash -s -- \\
  --database-url 'mysql://${DB_USER}:${DB_PASSWORD}@127.0.0.1:3306/${DB_NAME}' \\
  --listen 0.0.0.0:18080
EOF
}

main() {
  preflight
  log "installer=${INSTALLER_VERSION} db=${DB_NAME} user=${DB_USER} bind=${BIND_ADDRESS}"
  if [[ "$DRY_RUN" == "1" ]]; then
    log "dry-run passed"
    exit 0
  fi
  install_mysql_server
  ensure_root_access
  configure_bind_address
  create_database_user
  import_database
  verify_app_user
  print_summary
}

main "$@"
