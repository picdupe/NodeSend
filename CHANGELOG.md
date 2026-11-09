# Changelog

## [0.1.7] - 2026-11-09
### Added
- Add accent color customization and localized MSI packages
### Fixed
- Improve theme support and update handling, optimize transfer history
- Fix CLI output ordering and unsigned build update errors
- Refine window controls and page header masks

## [0.1.6] - 2026-11-04

### Added
- Added a shared background service and CLI commands.
- Added partial completion manifests and support for empty files.
- Updated the Node ID format and cache naming.
- Added Chinese, English, and Japanese localization, including system tray language switching.

### Improved
- Improved transfer status synchronization, reconnection, cache publishing, and file retransmission.
- Improved cache space handling and transfer recovery.

### Fixed
- Localized errors for changed, missing, or temporarily unavailable source files.
- Fixed cache path updates while preserving active task cache roots.
- Fixed update installation state lookup in the GUI process.

## [0.1.5] - 2026-10-29

### Added
- Added automatic update checks with version skipping and release notes.

### Fixed
- Fixed persistent transfer sessions and added pending receive notifications.

## [0.1.4] - 2026-10-24

### Improved
- Simplified the UI layout and enabled automatic settings saving.
- Optimized the overall app layout and title bar display.
- Improved sidebar interaction behavior.
- Improved Android MediaStore routing, pause/resume, cancellation cleanup, and task destination details.

### Added
- Added optional routing to Pictures, Movies, Music, and Download, with fallback to the configured receive directory.
- Added Android media permissions and an independent media routing setting.

### Fixed
- Fixed the CLI send API call.

### Breaking changes
- Legacy `Cancel` protocol frames are no longer accepted; cancellation uses `TaskState(cancelled)`.

## [0.1.3] - 2026-10-20

### Fixed
- Fixed update confirmation and installer behavior.
- Implemented single-instance activation and tray behavior.
- Showed file names in transfer details and removed the standalone timestamp.
- Improved transfer session UI and cancellation behavior.
- Fixed send confirmation flow for automatic acceptance.
- Fixed cache routing and manual IP transfer.
- Restored transfer state synchronization and cleanup.

### Added
- Added global receive confirmation and device detection.

### Security
- Blocked uninstall from drive roots and `ProgramFilesDir`.
- Skipped cache cleanup for protected directories.
- Restricted orphan cleanup to NodeSend task directories.
- Added a warning for unsafe installation root directories.

## [0.1.2] - 2026-10-17

### Improved
- Optimized transfer scanning and staged file commits.

## [0.1.1] - 2026-10-14

### Improved
- Optimized mechanical disk write performance.

### Fixed
- Fixed delayed file count updates during send task scanning.
