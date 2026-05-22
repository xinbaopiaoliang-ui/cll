#!/usr/bin/env bash
set -Eeuo pipefail

SERVICE_NAME="xaccel-control-api"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/xaccel-control-api"
LOG_DIR="/var/log/xaccel-control-api"
SYSTEMD_UNIT="/etc/systemd/system/${SERVICE_NAME}.service"
PURGE="0"

usage() {
  cat <<'USAGE'
Usage:
  control-api-uninstall.sh [--purge]

Options:
  --purge   Remove config and logs.
  -h        Show help.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --purge)
      PURGE="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

log() {
  printf '[xaccel-control-uninstall] %s\n' "$*"
}

if [[ "$(id -u)" != "0" ]]; then
  echo "[xaccel-control-uninstall] ERROR: please run as root" >&2
  exit 1
fi

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
  log "removed service; kept ${CONFIG_DIR} and ${LOG_DIR}"
fi

log "uninstalled"
