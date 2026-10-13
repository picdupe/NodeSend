#!/usr/bin/env bash
set -euo pipefail

# Build both the GUI AppImage and the native CLI on a Linux builder.
# Usage: ./scripts/linux.sh [debug]
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PYTHON="$(command -v python3 || true)"
if [[ -z "$PYTHON" ]]; then
  echo "Python 3 is required to synchronize the release version." >&2
  exit 127
fi
"$PYTHON" scripts/version.py sync

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust/Cargo is required to build NodeSend. Install it with:" >&2
  echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  echo "Then restart the shell or run: source \"\$HOME/.cargo/env\"" >&2
  exit 127
fi

if [[ "${1:-}" != "debug" ]]; then
  : "${TAURI_SIGNING_PRIVATE_KEY:?Set TAURI_SIGNING_PRIVATE_KEY to the updater signing key (keep it outside the repository) before a release build.}"
  NODESEND_UPDATER_PUBLIC_KEY="$(python3 -c 'import json; print(json.load(open("src-tauri/tauri.conf.json", encoding="utf-8"))["plugins"]["updater"]["pubkey"])')"
  export NODESEND_UPDATER_PUBLIC_KEY
  : "${NODESEND_UPDATER_PUBLIC_KEY:?The updater public key is missing from src-tauri/tauri.conf.json. Run scripts/signing.ps1 on Windows first.}"
fi

npm run build
# 先打包 GUI：nodesend-cli 属于 cli feature，不应进入 AppImage。
if [[ "${1:-}" == "debug" ]]; then
  PROFILE=debug
  CLI_FLAGS=()
  npm exec -- tauri build --bundles appimage --debug --config src-tauri/tauri.linux.fast.json
else
  PROFILE=release
  CLI_FLAGS=(--release)
  npm exec -- tauri build --bundles appimage
fi

# The Tauri updater uses the signed AppImage archive (.AppImage.tar.gz), not
# the directly runnable .AppImage. Add this platform to the same local manifest
# as Windows so one latest.json can be committed after all platform builds.
if [[ "$PROFILE" == "release" ]]; then
  VERSION="$(python3 -c 'import json; d=json.load(open("version.json", encoding="utf-8")); print(d["version"])')"
  BUILD="$(python3 -c 'import json; d=json.load(open("version.json", encoding="utf-8")); print(d["build"])')"
  INTERNAL_VERSION="${VERSION}-build.${BUILD}"
  PUBLIC_VERSION="v${VERSION}.${BUILD}"
  APPIMAGE_DIR="src-tauri/target/release/bundle/appimage"
  mapfile -t UPDATER_ARCHIVES < <(find "$APPIMAGE_DIR" -maxdepth 1 -type f -name '*.AppImage.tar.gz' -print | sort)
  if [[ ${#UPDATER_ARCHIVES[@]} -gt 0 ]]; then
    if [[ ${#UPDATER_ARCHIVES[@]} -ne 1 ]]; then
      echo "Expected one signed AppImage updater archive, found ${#UPDATER_ARCHIVES[@]}." >&2
      exit 1
    fi
    UPDATER_ARCHIVE="${UPDATER_ARCHIVES[0]}"
    UPDATER_SIGNATURE="${UPDATER_ARCHIVE}.sig"
    if [[ ! -s "$UPDATER_SIGNATURE" ]]; then
      echo "Cannot generate updater manifest: signature is missing or empty: $UPDATER_SIGNATURE" >&2
      exit 1
    fi
    ARCHIVE_NAME="NodeSend_${PUBLIC_VERSION}_x86_64.AppImage.tar.gz"
    if [[ "$UPDATER_ARCHIVE" != "$APPIMAGE_DIR/$ARCHIVE_NAME" ]]; then
      mv -f -- "$UPDATER_ARCHIVE" "$APPIMAGE_DIR/$ARCHIVE_NAME"
      mv -f -- "$UPDATER_SIGNATURE" "$APPIMAGE_DIR/$ARCHIVE_NAME.sig"
    fi
    DOWNLOAD_BASE="${NODESEND_UPDATE_DOWNLOAD_BASE_URL:-https://github.com/picdupe/NodeSend/releases/download/v${VERSION}}"
    DOWNLOAD_BASE="${DOWNLOAD_BASE%/}"
    export NODESEND_MANIFEST_VERSION="$INTERNAL_VERSION"
    export NODESEND_MANIFEST_PUBLIC_VERSION="$PUBLIC_VERSION"
    export NODESEND_MANIFEST_PLATFORM="linux-x86_64"
    export NODESEND_MANIFEST_SIGNATURE="$(cat "$APPIMAGE_DIR/$ARCHIVE_NAME.sig")"
    export NODESEND_MANIFEST_URL="${DOWNLOAD_BASE}/${ARCHIVE_NAME}"
    export NODESEND_MANIFEST_PATH="$ROOT/updates/stable/latest.json"
    python3 - <<'PY'
import json
import os
from pathlib import Path

path = Path(os.environ["NODESEND_MANIFEST_PATH"])
path.parent.mkdir(parents=True, exist_ok=True)
version = os.environ["NODESEND_MANIFEST_VERSION"]
platform = os.environ["NODESEND_MANIFEST_PLATFORM"]

manifest = {"version": version, "notes": "优化机械盘传输及大文件夹传输", "platforms": {}}
if path.exists():
    try:
        existing = json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SystemExit(f"Cannot merge existing updater manifest {path}: {exc}. Fix or remove the invalid JSON before building.")
    if existing.get("version") == version:
        platforms = existing.get("platforms", {})
        if not isinstance(platforms, dict):
            raise SystemExit(f"Cannot merge existing updater manifest {path}: 'platforms' must be an object.")
        manifest["platforms"].update(platforms)
        if "android" in existing:
            manifest["android"] = existing["android"]
        if isinstance(existing.get("notes"), str):
            manifest["notes"] = existing["notes"]
        print(f"Merging updater manifest for existing version/build: {version}")

manifest["platforms"][platform] = {
    "signature": os.environ["NODESEND_MANIFEST_SIGNATURE"],
    "url": os.environ["NODESEND_MANIFEST_URL"],
}
path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"Updater manifest generated: {path}")
print(f"Linux updater download URL: {os.environ['NODESEND_MANIFEST_URL']}")
PY
  else
    echo "Updater manifest skipped: no AppImage updater archive was produced in $APPIMAGE_DIR."
  fi
fi

# GUI 打包完成后再单独构建 CLI，作为独立命令分发。
cargo build --manifest-path src-tauri/Cargo.toml --features cli --bin nodesend-cli "${CLI_FLAGS[@]}"

mkdir -p dist/linux
cp "src-tauri/target/$PROFILE/nodesend-cli" dist/linux/
cp "src-tauri/target/$PROFILE/bundle/appimage/"*.AppImage dist/linux/ 2>/dev/null || true
echo "Linux artifacts written to dist/linux"
