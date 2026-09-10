# Changelog

## [0.2.4] - 2026-09-10

### Added

- Paste-last-transcript global shortcut.
- Case-sensitive custom-word entries with verbatim output.
- Official signed Windows and macOS release tooling.

### Changed

- Bumped handy-keys 0.2.4 to 0.3.4, along with related plugin and UI spacing updates.
- macOS release builds now require Apple Intelligence SDK support instead of silently falling back to stubs.
- Aligned the in-app updater with the published GitHub release metadata.

### Fixed

- Prevented a transcription deadlock that could leave the app stuck on Transcribing.
- Located `onnxruntime.dll` regardless of Windows bundle layout.

## [0.2.0] - 2026-05-04

### Added

- Renamed the application to MotsDits and reset project versioning to `0.2.0`.
- Added explicit platform boundaries for Windows, Linux, and shared core code.
- Added Windows build scripts that use a short Cargo target directory.
- Added Windows ONNX Runtime and DirectML bootstrap for ONNX-based models.
- Added deterministic custom-word aliases, blacklist support, and focused tests.

### Changed

- Windows Whisper now uses the GPU-capable Vulkan path.
- Linux keeps its native UI and platform-specific overlay behavior separate from
  the Windows React/Tauri UI.
- Update checks are disabled by default until signed updater metadata is
  published.
- GitHub Actions, docs, issue templates, and release artifacts now use MotsDits
  naming.

### Fixed

- Fixed Windows ONNX model loading getting stuck indefinitely by forcing a
  bundled ONNX Runtime DLL instead of relying on system DLL lookup.
- Fixed recordings getting stuck at `Transcribing...` when a model load is
  still in progress by adding a timeout guard.
- Fixed Windows stop/start recording state handling and overlay cleanup issues.
- Fixed custom-word alias behavior for Whisper and non-Whisper models.

## Upstream History

MotsDits is forked from Handy. Earlier Handy release history is preserved in the
upstream project; MotsDits tracks its own releases from `0.2.0` onward.
