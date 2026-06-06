# Changelog

All notable changes to TFD Bridge are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [SemVer](https://semver.org/).

---

## [Unreleased]

### Added
- Frameless dashboard UI matching `engine.tfd.rocks` design; custom topbar; TFD ship icon.
- Onboarding launch-on-login toggle (opt-in, persisted via store).
- Tray left-click now opens Battle Monitor directly.
- Auto-update client via signed GitHub Releases (`tauri-plugin-updater`). Update artifacts and `latest.json` are published as GitHub Release assets; the app checks `https://github.com/Helmi/tfd-bridge/releases/latest/download/latest.json` on startup.

---

## [0.1.2] — 2026-06-06

### Added
- Recursive replay subfolder listing: replays stored in nested directories under the configured path are now served.
- Safe nested fetch: path traversal is blocked for subfolder requests as well.

---

## [0.1.1] — 2026-06-05

### Added
- Live battle detection: `tempArenaInfo.json` is now included in the `/v1/replays` listing and served via `GET /v1/replays/tempArenaInfo.json` when a battle is in progress.

### Fixed
- `/v1/replays/latest` is now archive-only (no longer returns `tempArenaInfo.json`).

---

## [0.1.0] — 2026-06-05

Initial release.

### Added
- Tauri v2 desktop app: system tray, OS webview shell, single-instance enforcement.
- First-run onboarding: auto-detects WoWS replays folder (Steam / Wargaming Game Center paths); manual picker fallback.
- Localhost bridge (`127.0.0.1:43210`, fallback 43211–43214): read-only, strict CORS to `https://engine.tfd.rocks`.
  - `GET /v1/health` — version and capability info.
  - `GET /v1/replays` — directory listing.
  - `GET /v1/replays/:filename` — serve individual replay files.
- File-watch: bridge reloads listing on changes in the replays folder.
- NSIS installer; opt-in launch-on-login tray toggle.
- Tray "Open" menu item; bridge hot-starts immediately after onboarding completes.
- Optional "Open Battle Monitor" tray item (opens `engine.tfd.rocks/monitor` in the browser).
- Windows build and release CI (GitHub Actions, `tauri-action`).

### Security
- `resolve_safe_path` rejects symlinks and path traversal in served files.
- Onboarding uses `createElement`/`textContent` (no `innerHTML`) to prevent injection.

[Unreleased]: https://github.com/Helmi/tfd-bridge/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/Helmi/tfd-bridge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Helmi/tfd-bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Helmi/tfd-bridge/releases/tag/v0.1.0
