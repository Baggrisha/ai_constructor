#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
./scripts/generate-bindings.sh
xcodebuild -project apps/macos/AIConstructor.xcodeproj -scheme AIConstructor \
  -configuration Debug -derivedDataPath "$ROOT/build/DerivedData" \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO build
mkdir -p build
if [ -e build/AIConstructor.app ]; then
  printf '%s\n' 'build/AIConstructor.app already exists; move it before rebuilding to avoid overwriting it.' >&2
  exit 1
fi
ditto build/DerivedData/Build/Products/Debug/AIConstructor.app build/AIConstructor.app
printf 'Application: %s\n' "$ROOT/build/AIConstructor.app"
