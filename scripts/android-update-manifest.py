#!/usr/bin/env python3
"""Merge Android APK metadata into the shared per-version update manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--channel", choices=("stable", "test"), required=True)
    parser.add_argument("--apk", type=Path, required=True, help="Path to the signed APK produced by this build")
    parser.add_argument("--url", required=True, help="Public HTTPS URL where this APK will be published")
    parser.add_argument("--fallback-url", action="append", default=[], help="Fallback HTTPS URL; may be repeated")
    args = parser.parse_args()

    if not args.url.startswith("https://"):
        raise SystemExit("APK URL must use HTTPS.")
    if any(not url.startswith("https://") for url in args.fallback_url):
        raise SystemExit("All APK fallback URLs must use HTTPS.")
    apk = args.apk.resolve()
    if not apk.is_file() or apk.suffix.lower() != ".apk":
        raise SystemExit("--apk must point to an existing .apk file.")

    version_data = json.loads((ROOT / "version.json").read_text(encoding="utf-8"))
    version = version_data["version"]
    build = int(version_data["build"])
    major, minor, patch = (int(part) for part in version.split("."))
    version_code = major * 100_000_000 + minor * 1_000_000 + patch * 10_000 + build
    digest = hashlib.sha256()
    with apk.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)

    internal_version = f"{version}-build.{build}"
    manifest_path = ROOT / "updates" / args.channel / "latest.json"
    if manifest_path.exists():
        try:
            existing = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
        except (OSError, json.JSONDecodeError) as exc:
            raise SystemExit(f"Cannot merge existing updater manifest {manifest_path}: {exc}. Fix the JSON before building.")
        if not isinstance(existing, dict):
            raise SystemExit(f"Cannot merge existing updater manifest {manifest_path}: root must be an object.")
        if existing.get("version") == internal_version:
            manifest = existing
            platforms = manifest.get("platforms", {})
            if not isinstance(platforms, dict):
                raise SystemExit(f"Cannot merge existing updater manifest {manifest_path}: 'platforms' must be an object.")
            print(f"Merging updater manifest for existing version/build: {internal_version}")
        else:
            manifest = {"version": internal_version, "notes": "", "platforms": {}}
    else:
        manifest_path.parent.mkdir(parents=True, exist_ok=True)
        manifest = {"version": internal_version, "notes": "", "platforms": {}}

    manifest.setdefault("version", internal_version)
    manifest.setdefault("notes", "")
    manifest.setdefault("platforms", {})
    manifest["android"] = {
        "version": f"{version}.{build}",
        "versionCode": version_code,
        "url": args.url,
        "fallbackUrls": args.fallback_url,
        "sha256": digest.hexdigest(),
    }
    manifest["release_page_url"] = "https://gitcode.com/picdupe/NodeSend/releases"
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Updated {manifest_path.relative_to(ROOT)}")
    print(f"Android version: {version}.{build}; versionCode: {version_code}")
    print(f"APK SHA-256: {digest.hexdigest()}")
    print("Commit and publish this manifest after the APK is publicly available.")


if __name__ == "__main__":
    main()
