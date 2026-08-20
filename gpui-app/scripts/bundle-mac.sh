#!/usr/bin/env bash
# Builds the gpui-app release binary, wraps it into a double-clickable
# Graviz.app bundle (+ a drag-to-Applications .dmg), and reinstalls it into
# /Applications so the newest build is always the one that launches.
#
# Usage: scripts/bundle-mac.sh
# Output: dist/Graviz.app, dist/Graviz.dmg, /Applications/Graviz.app

set -euo pipefail

APP_NAME="Graviz"
BIN_NAME="graviz"
BUNDLE_ID="com.justive.graviz"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT_DIR/.." && pwd)"
ICON_SRC="$REPO_ROOT/src/icon-512.png"

DIST_DIR="$ROOT_DIR/dist"
APP_DIR="$DIST_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

echo "==> cargo build --release"
cd "$ROOT_DIR"
cargo build --release

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

echo "==> assembling $APP_NAME.app"
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "target/release/$BIN_NAME" "$MACOS_DIR/$APP_NAME"

if [ -f "$ICON_SRC" ]; then
  echo "==> generating AppIcon.icns from $ICON_SRC"
  ICONSET="$DIST_DIR/AppIcon.iconset"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  for size in 16 32 64 128 256 512; do
    sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$RESOURCES_DIR/AppIcon.icns"
  rm -rf "$ICONSET"
else
  echo "==> WARNING: $ICON_SRC not found, skipping app icon"
fi

cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>
  <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>CFBundleExecutable</key>
  <string>$APP_NAME</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon.icns</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSHumanReadableCopyright</key>
  <string>Copyright</string>
</dict>
</plist>
PLIST

echo "==> ad-hoc codesigning"
codesign --deep --force --sign - "$APP_DIR"

echo "==> building $APP_NAME.dmg"
DMG_STAGE="$DIST_DIR/dmg-stage"
rm -rf "$DMG_STAGE"
mkdir -p "$DMG_STAGE"
cp -R "$APP_DIR" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"
rm -f "$DIST_DIR/$APP_NAME.dmg"
hdiutil create -volname "$APP_NAME" -srcfolder "$DMG_STAGE" -ov -format UDZO "$DIST_DIR/$APP_NAME.dmg" >/dev/null
rm -rf "$DMG_STAGE"

echo "==> reinstalling into /Applications"
pkill -f "/$APP_NAME.app/Contents/MacOS/$APP_NAME" 2>/dev/null && sleep 1 || true
rm -rf "/Applications/$APP_NAME.app"
cp -R "$APP_DIR" /Applications/

echo "==> done"
echo "    $APP_DIR"
echo "    $DIST_DIR/$APP_NAME.dmg"
echo "    /Applications/$APP_NAME.app"
