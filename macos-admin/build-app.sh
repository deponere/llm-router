#!/usr/bin/env bash
# Builds RouterAdmin.app (a menu-bar agent) from the SwiftPM executable and the
# Rust helper binaries. Output: macos-admin/RouterAdmin.app
set -euo pipefail
cd "$(dirname "$0")"
ROOT="$(cd .. && pwd)"

echo "==> building Rust helper + router (release)"
( cd "$ROOT" && cargo build --release -p router-admin -p router-api )

echo "==> building Swift app (release)"
swift build -c release

BIN="$(swift build -c release --show-bin-path)/RouterAdmin"
APP="RouterAdmin.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp Info.plist "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/RouterAdmin"

# Ad-hoc sign so macOS lets it run locally (unsigned menu-bar agents get killed).
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true

echo "==> done: $(pwd)/$APP"
echo "    open with:  open $APP"
