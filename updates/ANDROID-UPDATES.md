# Android in-app updates

Android updates use the existing `updates/<channel>/latest.json` manifest, but
they do not use Tauri's desktop updater signature format. Each published APK
must be signed with the same Android keystore identity as the installed app.
Android's package installer enforces the package/signing-certificate match.

The release build script (`scripts/android.ps1`, without `-Fast` or `-Debug`)
now performs this automatically. It renames the signed release APK to a
versioned filename, calculates SHA-256 from that exact local APK, and merges
the Android entry into `updates/stable/latest.json`. The download URL follows
the same `NODESEND_UPDATE_DOWNLOAD_BASE_URL` setting as the other platform
builds, or defaults to the GitHub release for the product version. Upload the
APK under the printed filename to that URL before publishing the manifest.

The helper can also be run manually when repairing release metadata:

```powershell
python scripts/android-update-manifest.py --channel stable --apk "path\to\NodeSend.apk" --url "https://example.invalid/NodeSend.apk" --notes "Release notes"
```

Replace the example URL with the real public HTTPS URL. The script computes
SHA-256 from the APK and derives `versionCode` from `version.json`; it does not
increment the build number. When the manifest already has the same internal
version/build, the script preserves existing platform entries and metadata and
updates only the Android entry. A different version/build starts a fresh
manifest, matching the Windows build behavior. Commit and publish the resulting
manifest so the app can read it from the repository's raw-content URL.

The manifest's `android` object contains `version`, `versionCode`, `url`, and
`sha256`. The Android client downloads to its private cache, verifies SHA-256,
and asks Android's system package installer to perform the upgrade. The user
may need to allow installs from NodeSend in system settings and must confirm the
installation. There is no silent installation.
