#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
./scripts/generate-bindings.sh
SDK="$(xcrun --sdk macosx --show-sdk-path)"
APP="$ROOT/build/AIConstructor.app"
if [ -e "$APP" ]; then
  printf '%s\n' 'build/AIConstructor.app already exists; move it before rebuilding to avoid overwriting it.' >&2
  exit 1
fi
mkdir -p "$ROOT/build"
STAGE="$(mktemp -d "$ROOT/build/.aic-build.XXXXXX")"
trap 'rm -rf -- "$STAGE"' EXIT
STAGED_APP="$STAGE/AIConstructor.app"
mkdir -p "$STAGED_APP/Contents/MacOS"
cp apps/macos/Info.plist "$STAGED_APP/Contents/Info.plist"
xcrun swiftc -swift-version 5 -parse-as-library -target arm64-apple-macosx14.0 \
  -sdk "$SDK" -I "$ROOT/apps/macos/Generated" \
  apps/macos/AIConstructor/AIConstructorApp.swift apps/macos/Generated/aic_bridge.swift \
  "$ROOT/target/debug/libaic_bridge.a" \
  -framework AppKit -framework SwiftUI -framework Security -framework CoreFoundation \
  -lc++ -lresolv -o "$STAGED_APP/Contents/MacOS/AIConstructor"
if [ -e "$APP" ]; then
  printf '%s\n' 'build/AIConstructor.app appeared during the build; refusing to overwrite it.' >&2
  exit 1
fi
mv "$STAGED_APP" "$APP"
printf 'Application: %s\n' "$APP"
