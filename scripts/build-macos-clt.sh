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
SOURCES=(
  apps/macos/AIConstructor/App/AIConstructorApp.swift
  apps/macos/AIConstructor/App/WindowLayout.swift
  apps/macos/AIConstructor/Model/SnapshotModel.swift
  apps/macos/AIConstructor/Model/ProjectLibrary.swift
  apps/macos/AIConstructor/Model/WorkspaceModel.swift
  apps/macos/AIConstructor/Views/ProjectBrowser.swift
  apps/macos/AIConstructor/Views/ChatsPanel.swift
  apps/macos/AIConstructor/Views/AssetsPanel.swift
  apps/macos/AIConstructor/Views/DiagnosticsPanel.swift
  apps/macos/AIConstructor/Views/Components.swift
  apps/macos/AIConstructor/Views/Canvas/CanvasView.swift
  apps/macos/AIConstructor/Views/Canvas/CanvasWebView.swift
  apps/macos/AIConstructor/Views/Canvas/CanvasOverlay.swift
  apps/macos/AIConstructor/Views/Sheets/ConflictResolutionSheet.swift
  apps/macos/AIConstructor/Views/Sheets/RecoveryResolutionSheet.swift
)
missing=0
for source in "${SOURCES[@]}"; do
  if [ ! -f "$source" ]; then
    printf 'Missing source: %s\n' "$source" >&2
    missing=1
  fi
done
[ "$missing" -eq 0 ] || exit 1
xcrun swiftc -swift-version 5 -parse-as-library -target arm64-apple-macosx14.0 \
  -sdk "$SDK" -I "$ROOT/apps/macos/Generated" \
  "${SOURCES[@]}" apps/macos/Generated/aic_bridge.swift \
  "$ROOT/target/debug/libaic_bridge.a" \
  -framework AppKit -framework SwiftUI -framework Security -framework CoreFoundation -framework WebKit \
  -lc++ -lresolv -o "$STAGED_APP/Contents/MacOS/AIConstructor"
if [ -e "$APP" ]; then
  printf '%s\n' 'build/AIConstructor.app appeared during the build; refusing to overwrite it.' >&2
  exit 1
fi
mv "$STAGED_APP" "$APP"
printf 'Application: %s\n' "$APP"
