#!/usr/bin/env bash
set -euo pipefail

# Build both the GUI AppImage and the native CLI on a Linux builder.
# Usage: ./scripts/linux.sh [debug]
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

npm run build
if [[ "${1:-}" == "debug" ]]; then
  cargo build --manifest-path src-tauri/Cargo.toml --bin nodesend-cli
  cargo tauri build --bundles appimage --debug
else
  cargo build --manifest-path src-tauri/Cargo.toml --bin nodesend-cli --release
  cargo tauri build --bundles appimage
fi

mkdir -p dist/linux
cp src-tauri/target/release/nodesend-cli dist/linux/ 2>/dev/null || true
cp src-tauri/target/debug/nodesend-cli dist/linux/ 2>/dev/null || true
cp src-tauri/target/release/bundle/appimage/*.AppImage dist/linux/ 2>/dev/null || true
cp src-tauri/target/debug/bundle/appimage/*.AppImage dist/linux/ 2>/dev/null || true
echo "Linux artifacts written to dist/linux"
