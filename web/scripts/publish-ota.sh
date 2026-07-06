#!/usr/bin/env bash
#
# Publish a versioned OTA bundle of the built PWA to MinIO.
#
# The PWA ships as an over-the-air (OTA) bundle: vite-plugin-pwa's autoUpdate
# service worker refreshes clients to the newest precache on next launch, and
# native Capacitor shells pull the same bundle from object storage. This script
# uploads dist/ under an immutable, version-pinned prefix and moves a `latest`
# pointer + manifest so a rollback is just re-pointing `latest`.
#
# Layout in the bucket:
#   <app>/bundles/<version>/...            the immutable bundle (dist verbatim)
#   <app>/bundles/<version>/manifest.json  {app, version, sha, builtAt, files}
#   <app>/latest.json                      pointer → the current version
#
# Required env:
#   MINIO_ENDPOINT      e.g. https://minio.vforce360.ai
#   MINIO_ACCESS_KEY / MINIO_SECRET_KEY
# Optional env:
#   OTA_BUCKET   (default: made-ota)
#   APP_NAME     (default: made-pwa)
#   GIT_SHA      (default: `git rev-parse --short HEAD`)
#   DIST_DIR     (default: dist)
#
# Requires the MinIO client `mc` on PATH.
set -euo pipefail

: "${MINIO_ENDPOINT:?MINIO_ENDPOINT is required}"
: "${MINIO_ACCESS_KEY:?MINIO_ACCESS_KEY is required}"
: "${MINIO_SECRET_KEY:?MINIO_SECRET_KEY is required}"

OTA_BUCKET="${OTA_BUCKET:-made-ota}"
APP_NAME="${APP_NAME:-made-pwa}"
DIST_DIR="${DIST_DIR:-dist}"
GIT_SHA="${GIT_SHA:-$(git rev-parse --short HEAD)}"

if [[ ! -d "$DIST_DIR" ]]; then
  echo "publish-ota: dist dir '$DIST_DIR' not found — build the bundle first" >&2
  exit 1
fi

# Version = package.json version + short sha, so every commit is a distinct,
# immutable bundle (e.g. 0.1.0+1a2b3c4).
PKG_VERSION="$(node -p "require('./package.json').version")"
VERSION="${PKG_VERSION}+${GIT_SHA}"
BUILT_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
FILE_COUNT="$(find "$DIST_DIR" -type f | wc -l | tr -d ' ')"

DEST="local/${OTA_BUCKET}/${APP_NAME}/bundles/${VERSION}"

echo "publish-ota: publishing ${APP_NAME} ${VERSION} (${FILE_COUNT} files) → ${OTA_BUCKET}"

# Configure the alias (credentials never printed).
mc alias set local "$MINIO_ENDPOINT" "$MINIO_ACCESS_KEY" "$MINIO_SECRET_KEY" >/dev/null
# Idempotent: create the bucket if a first-ever publish.
mc mb --ignore-existing "local/${OTA_BUCKET}" >/dev/null

# Per-version manifest, written alongside the bundle.
cat > "${DIST_DIR}/manifest.json" <<JSON
{
  "app": "${APP_NAME}",
  "version": "${VERSION}",
  "sha": "${GIT_SHA}",
  "builtAt": "${BUILT_AT}",
  "files": ${FILE_COUNT}
}
JSON

# Immutable upload of the whole bundle under the version prefix.
mc cp --recursive "${DIST_DIR}/" "${DEST}/"

# Move the `latest` pointer last, so a partially-uploaded bundle is never live.
LATEST_JSON="$(mktemp)"
cat > "$LATEST_JSON" <<JSON
{
  "app": "${APP_NAME}",
  "version": "${VERSION}",
  "sha": "${GIT_SHA}",
  "builtAt": "${BUILT_AT}",
  "bundle": "${APP_NAME}/bundles/${VERSION}/"
}
JSON
mc cp "$LATEST_JSON" "local/${OTA_BUCKET}/${APP_NAME}/latest.json"
rm -f "$LATEST_JSON"

echo "publish-ota: done. latest → ${APP_NAME}/bundles/${VERSION}/"
