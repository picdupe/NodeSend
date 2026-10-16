#!/usr/bin/env python3
"""Synchronize NodeSend version metadata or manually increment its build number."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
VERSION_FILE = ROOT / "version.json"


def read_version() -> tuple[str, int, str, int]:
    data = json.loads(VERSION_FILE.read_text(encoding="utf-8"))
    version = data.get("version")
    build = data.get("build")
    if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise SystemExit('version.json: "version" must use MAJOR.MINOR.PATCH, e.g. "3.4.8".')
    if isinstance(build, bool) or not isinstance(build, int) or not 1 <= build <= 999:
        raise SystemExit('version.json: "build" must be an integer from 1 to 999.')
    major, minor, patch = (int(part) for part in version.split("."))
    if major > 20 or minor > 99 or patch > 99:
        raise SystemExit("Version must fit the Android versionCode scheme (major <= 20, minor/patch <= 99).")
    semver = f"{version}-build.{build}"
    android_code = major * 100_000_000 + minor * 1_000_000 + patch * 10_000 + build
    return version, build, semver, android_code


def replace_one(path: Path, pattern: str, replacement: str, description: str) -> None:
    content = path.read_text(encoding="utf-8")
    updated, count = re.subn(pattern, replacement, content, count=1, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"Could not update {description} in {path.relative_to(ROOT)} (matches: {count}).")
    if updated != content:
        path.write_text(updated, encoding="utf-8", newline="")


def sync() -> None:
    version, build, semver, android_code = read_version()
    # Android's user-visible version must match the Windows release label and
    # must not expose the internal SemVer build prerelease suffix.
    label = f"{version}.{build}"

    replace_one(ROOT / "package.json", r'("version"\s*:\s*")[^"]+("\s*,)', rf'\g<1>{semver}\2', "package.json version")
    replace_one(ROOT / "package-lock.json", r'("name": "nodesend",\s*"version": ")[^"]+', rf'\g<1>{semver}', "package-lock root version")
    replace_one(ROOT / "package-lock.json", r'("": \{\s*"name": "nodesend",\s*"version": ")[^"]+', rf'\g<1>{semver}', "package-lock package version")
    replace_one(ROOT / "src-tauri/tauri.conf.json", r'("version"\s*:\s*")[^"]+("\s*,)', rf'\g<1>{semver}\2', "Tauri version")
    replace_one(ROOT / "src-tauri/Cargo.toml", r'(?m)^(version\s*=\s*")[^"]+(")', rf'\g<1>{semver}\2', "Cargo package version")
    replace_one(ROOT / "src-tauri/Cargo.lock", r'(?m)(^name = "nodesend"\s+version = ")[^"]+(\")', rf'\g<1>{semver}\2', "Cargo lock package version")
    replace_one(ROOT / "src-tauri/gen/android/app/tauri.properties", r'(?m)^tauri\.android\.versionName=.*$', f"tauri.android.versionName={label}", "Android version name")
    replace_one(ROOT / "src-tauri/gen/android/app/tauri.properties", r'(?m)^tauri\.android\.versionCode=.*$', f"tauri.android.versionCode={android_code}", "Android version code")
    print(f"NodeSend version synchronized: {label} (internal: {semver}; Android versionCode: {android_code})")


def increment() -> None:
    data = json.loads(VERSION_FILE.read_text(encoding="utf-8"))
    _, build, _, _ = read_version()
    if build >= 999:
        raise SystemExit("Build number reached 999. Set a new product version and reset build to 1 in version.json.")
    data["build"] = build + 1
    VERSION_FILE.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="")
    sync()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("sync", "increment"), help="sync metadata or manually increment the build number")
    args = parser.parse_args()
    if args.action == "sync":
        sync()
    else:
        increment()


if __name__ == "__main__":
    main()
