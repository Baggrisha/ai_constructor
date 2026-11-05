#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
./scripts/generate-bindings.sh
xcodebuild -project apps/macos/AIConstructor.xcodeproj -scheme AIConstructorUITests \
  -configuration Debug -derivedDataPath "$ROOT/build/DerivedData" \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO test
