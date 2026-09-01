#!/usr/bin/env bash
# Builds the app (via bundle-mac.sh), then publishes it as a GitHub Release
# tagged with the version in Cargo.toml — the thing the in-app update check
# (src/update_check.rs) polls for.
#
# Usage: scripts/release.sh

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="v$VERSION"

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "==> $TAG already exists — bump the version in Cargo.toml first" >&2
  exit 1
fi

echo "==> building $TAG"
"$ROOT_DIR/scripts/bundle-mac.sh"

echo "==> tagging $TAG"
git tag -a "$TAG" -m "$TAG"
git push origin "$TAG"

echo "==> creating GitHub release"
gh release create "$TAG" "$ROOT_DIR/dist/Graviz.dmg" \
  --title "Graviz $TAG" \
  --generate-notes

echo "==> done: https://github.com/JUSTIVE/Graviz/releases/tag/$TAG"
