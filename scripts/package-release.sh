#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_DIR="${ROOT_DIR}/node-core"
DIST_DIR="${ROOT_DIR}/dist"

VERSION="$(grep -m1 '^version =' "${NODE_DIR}/Cargo.toml" | sed -E 's/version = "([^"]+)"/\1/')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64)
    RELEASE_ARCH="x86_64"
    ;;
  aarch64|arm64)
    RELEASE_ARCH="aarch64"
    ;;
  *)
    echo "unsupported arch: $ARCH" >&2
    exit 1
    ;;
esac

ARTIFACT_NAME="xaccel-node-${VERSION}-linux-${RELEASE_ARCH}"
GENERIC_ARTIFACT_NAME="xaccel-node-linux-${RELEASE_ARCH}"
WORK_DIR="${DIST_DIR}/${ARTIFACT_NAME}"
BUILD_TARGET="${CARGO_BUILD_TARGET:-}"
BINARY_DIR="${NODE_DIR}/target/release"

if [[ -n "$BUILD_TARGET" ]]; then
  BINARY_DIR="${NODE_DIR}/target/${BUILD_TARGET}/release"
fi

rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR" "$DIST_DIR"

cd "$NODE_DIR"
if [[ -n "$BUILD_TARGET" ]]; then
  cargo build --release --locked --target "$BUILD_TARGET"
else
  cargo build --release --locked
fi

cp "${BINARY_DIR}/xaccel-node" "$WORK_DIR/"
cp "${ROOT_DIR}/install/config.example.toml" "$WORK_DIR/"
cp "${ROOT_DIR}/install/systemd/xaccel-node.service" "$WORK_DIR/"
cp "${ROOT_DIR}/install/uninstall.sh" "$WORK_DIR/"

cd "$DIST_DIR"
tar -czf "${ARTIFACT_NAME}.tar.gz" "$ARTIFACT_NAME"
cp "${ARTIFACT_NAME}.tar.gz" "${GENERIC_ARTIFACT_NAME}.tar.gz"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${ARTIFACT_NAME}.tar.gz" > "${ARTIFACT_NAME}.sha256"
  sha256sum "${GENERIC_ARTIFACT_NAME}.tar.gz" > "${GENERIC_ARTIFACT_NAME}.tar.gz.sha256"
else
  shasum -a 256 "${ARTIFACT_NAME}.tar.gz" > "${ARTIFACT_NAME}.sha256"
  shasum -a 256 "${GENERIC_ARTIFACT_NAME}.tar.gz" > "${GENERIC_ARTIFACT_NAME}.tar.gz.sha256"
fi

echo "created ${DIST_DIR}/${ARTIFACT_NAME}.tar.gz"
echo "created ${DIST_DIR}/${ARTIFACT_NAME}.sha256"
echo "created ${DIST_DIR}/${GENERIC_ARTIFACT_NAME}.tar.gz"
echo "created ${DIST_DIR}/${GENERIC_ARTIFACT_NAME}.tar.gz.sha256"
