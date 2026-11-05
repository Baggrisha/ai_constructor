#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export MACOSX_DEPLOYMENT_TARGET=14.0
cargo build --locked -p aic-bridge --lib
mkdir -p apps/macos/Generated
cargo run --locked -p aic-bridge --bin uniffi-bindgen -- generate \
  --library "$ROOT/target/debug/libaic_bridge.dylib" \
  --language swift --out-dir "$ROOT/apps/macos/Generated"
cp apps/macos/Generated/aic_bridgeFFI.modulemap apps/macos/Generated/module.modulemap
