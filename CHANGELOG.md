# Changelog

All notable changes to TFD Bridge are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [SemVer](https://semver.org/).

---

## [Unreleased]

---

## [0.2.1] — 2026-06-06

### Fixed
- The app could not be quit and had to be killed via Task Manager — most noticeable after opening Battle Monitor. Quit (from the tray menu) now reliably exits the app.

---

## [0.2.0] — 2026-06-06

### Added
- Frameless dashboard UI matching the `engine.tfd.rocks` look, with a custom topbar (drag to move, minimize, close-to-tray) and a TFD ship icon.
- Onboarding **Launch on login (Recommended)** option — pre-checked, persisted, and kept in sync with the tray toggle.
- **Left-click the tray icon** now opens Battle Monitor directly (right-click still shows the menu).
- Auto-update: the app checks for updates on launch and self-updates from signed GitHub Release artifacts (`tauri-plugin-updater`, ed25519-signed; `latest.json` published on GitHub Releases).

### Changed
- Releases are now published as full GitHub releases (no longer pre-releases) so the updater feed (`/releases/latest/`) resolves.

### Security
- The bridge serves only replay files on fetch (`*.wowsreplay`, `tempArenaInfo.json`); any other file inside the folder returns `404`.
- Release builds enforce strict CORS to `https://engine.tfd.rocks` (the dev-origin override is debug-build only).
- Non-`GET` requests now return `405`.

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
- Tauri v2 desktop app: system tray and OS webview shell.
- First-run onboarding: auto-detects the WoWS replays folder (Steam / Wargaming Game Center paths); manual picker fallback.
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

[Unreleased]: https://github.com/Helmi/tfd-bridge/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/Helmi/tfd-bridge/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Helmi/tfd-bridge/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/Helmi/tfd-bridge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Helmi/tfd-bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Helmi/tfd-bridge/releases/tag/v0.1.0
