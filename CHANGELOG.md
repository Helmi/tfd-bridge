# Changelog

All notable changes to TFD Bridge are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [SemVer](https://semver.org/).

---

## [Unreleased]

### Fixed
- **Post-battle plane-kills column showed garbage (e.g. `10000`).** The per-player aircraft-destroyed value was read from the `RIBBON_PLANE` ribbon, which is populated only for the recording player and only ever as a `10000` sentinel (it is `0` for everyone else) — never a real count. The battle-result API now exposes **`planes_killed`** = `planes_killed_by_ship` (AA) + `planes_killed_by_plane` (carrier aircraft), a public per-player count present and correct for **all** players. Validated against 212 real players across 15.4/15.5 reference replays (100% match). The old `ribbons_plane_kills` field is removed in favour of `planes_killed` (which the engine already prefers). (td-4b4c1a)

### Changed
- **WoWS 15.5 is now a known-good decode version.** Added `(15, 5)` to the known-good set, so 15.5 replays no longer carry the `known_good_version` "field mapping may be stale" warning. Confirmed by decoding 5 real 15.5 **PvP** battles end-to-end (`decode_status = ok`, full per-player field cross-check passes across 212 players). 15.5 shares the 15.3/15.4 positional layout; no constants/parser change was required (upstream `wows-toolkit` has only an unrelated GUI export change since the pinned rev). (Co-op/PVE battles can still read `decode_status = unreliable` via the PvP-tuned win-XP-multiplier check — a pre-existing behaviour, unchanged here.)

---

## [0.5.0] — 2026-06-16

### Added
- **Full per-target damage breakdown in the battle-result API.** Each player in `GET /v1/replays/{name}/result` (and `/latest/result`) now carries an `interactions` array — the attacker→victim matrix the decoder previously discarded. Per target it reports damage split by weapon type (main battery, secondary, torpedo, aircraft, fire, flood, ram, depth charge, other), spotting damage, fires / floods / crits / citadels, the killing blow, and the first-spot, sorted by damage dealt. "Damage *received* from a ship" is the transpose: find that ship's interaction whose `target_id` is you. This is the data a full post-battle damage-analysis result screen needs. The result `schema_version` is now `1.1` (purely additive — every existing field is unchanged).
- **Self-validating decode (`meta.decode_status` + `meta.decode_checks`).** Every decoded result is now checked against the values we *expect*, and graded by how far it deviates — so a future game patch that silently shifts the replay's positional field layout is caught instead of served as plausible-but-wrong numbers on a normal `200`. `decode_status` (always present) is `ok` / `degraded` / `unreliable`; `decode_checks` lists each expectation that failed with its `severity` and an expected-vs-actual `detail`. The checks span structural anchors (player-array length, the account-id anchor at index 0, the exp/raw_exp win-multiplier ≈1.5/1.0), per-field domain ranges (team id ∈ {0,1}, ship tier 1–11, frags 0–12, hits ≤ shots fired, no negative damage/XP, plausible raw-XP), the game-version known-good gate, ship-resolution rate, and a cross-field reconciliation of the new interaction matrix against total damage received. A handful of outliers reads as `degraded`; a *systematic* break (the signature of a layout shift) reads as `unreliable`, so the result screen can show "decode unreliable — update needed" rather than rendering corrupt stats. (WoWS 15.3 and 15.4 are confirmed layout-identical, so the bundled mapping is correct for both today.)

### Security
- The new damage matrix and decode-status are produced entirely by the existing in-process, loopback-only decoder — no new data is sent anywhere.

---

## [0.4.1] — 2026-06-15

### Added
- **Change replays folder from the dashboard.** A folder-icon button now sits next to the replays-path display, opening the native folder picker so you can re-point the bridge at a different replays folder without reinstalling or editing config by hand. Previously the folder could only be set during first-run onboarding.
- **Window maximize/restore button.** The frameless title bar has a new maximize/restore control between minimize and close. On Windows 10 the title-bar double-click does not maximize a frameless window, so this button is the reliable way to maximize; its icon and tooltip reflect the current state.

---

## [0.4.0] — 2026-06-14

### Added
- **Local battle-result decoding.** When a battle finishes, the bridge can now read detailed post-battle statistics straight out of the `.wowsreplay` file — data the public API does not expose — and serve it to the Battle Monitor, so a post-battle result screen can render with no server round-trip. New loopback endpoints `GET /v1/replays/{name}/result` and `GET /v1/replays/latest/result` return a versioned JSON result, and `/v1/health` advertises a new `battle-result-v1` capability when decoding is available.
  - Per player: ship (by id) with tier/class, team and division, base and earned XP, damage dealt / potential / received, spotting damage, main-battery shots and hits, kills, torpedo / plane / hit ribbons, and survived / won / afk flags. Your own row additionally includes credits earned — a replay contains economics only for the local player, never for other players.
  - Decoding runs entirely in-process via the `wows_replays` library (a pinned fork of [landaire/wows-toolkit](https://github.com/landaire/wows-toolkit), MIT) — no bundled executable — plus bundled ship and field-mapping reference data (JSON).

### Changed
- `GET /v1/replays/latest/result` returns the newest *finished* battle that has results — the live in-progress file and battles you left early are skipped.
- Incomplete replays are handled cleanly: if you left a battle early or it is still in progress, the result endpoints return `404` "no battle result" instead of an error. Replays from game versions older than WoWS 15.3 cannot yet be decoded by the bundled decoder.

### Security
- **Decoding runs entirely on your machine.** The decoded result is served only to the local Battle Monitor over loopback (`127.0.0.1`); this feature sends no new data anywhere. (The separate, opt-in replay donation is unchanged.)
- The new endpoints reuse the existing loopback-only binding, `Host`-header (DNS-rebinding) check, strict CORS to `engine.tfd.rocks`, and path-traversal validation. Decoding runs in-process (no external executable); parsing of untrusted replay bytes is isolated behind a panic boundary — a known, accepted trust boundary, as with replay donation.

---

## [0.3.1] — 2026-06-12

### Fixed
- **Replay donation now actually uploads.** In 0.3.0 the opt-in donation feature silently rejected every replay: a byte-order bug in the `.wowsreplay` validity check treated genuine replay files as malformed, so nothing was ever sent — not newly finished battles, and not the existing-replay backfill. The check now recognises real replay files, so donations (opt-in, anonymous) upload as intended once you've opted in.

---

## [0.3.0] — 2026-06-11

### Added
- **Replay donation (opt-in).** Donate your replays to the TFD community archive — the app asks once, and you can change your answer any time in settings.
- **App version** shown in the dashboard title bar and the tray menu.
- **Remote configuration.** The app checks `engine.tfd.rocks` hourly — replay donation can be paused server-side in case of problems, and an update prompt appears when the server requires a newer bridge version.

### Security
- The loopback bridge validates the `Host` header, blocking DNS-rebinding attacks against the local replay server.
- Restrictive Content-Security-Policy for the bundled dashboard UI.

---

## [0.2.4] — 2026-06-08

### Added
- **Window size and position are remembered.** The app reopens at the size, position, and maximized state you left it in, instead of always opening at the default 820×600.
- **Hourly update checks** while the app is running, plus a tray **"Check for updates now"** item that reports the result (including when you're already up to date).
- **Last view is restored on launch.** If Battle Monitor was open when you closed the app, it reopens on the monitor; otherwise it opens on the dashboard. (The monitor view is only restored once onboarding is complete.)

### Fixed
- **Battle Monitor no longer over-scrolls** past the bottom by the height of the top bar — content now sits flush below the bar with no dead gap.

---

## [0.2.3] — 2026-06-06

### Added
- Battle Monitor view now has **full window controls** — drag to move, minimize, and close — so the window behaves the same whether you're on the dashboard or the monitor.
- Diagnostic **file logging** (written to the app log folder) so issues can be investigated without a special build.

### Changed
- Reworked the Battle Monitor **"← Dashboard"** navigation to return to the exact dashboard URL.

---

## [0.2.2] — 2026-06-06

### Changed
- Battle Monitor now opens **inside the main window** (no separate window), with a "← Dashboard" bar to go back. Consistent frameless look with the dashboard.

### Fixed
- Links in Battle Monitor that open a new tab now open in your **default browser** instead of doing nothing.

### Security
- The external-link opener only accepts `http`/`https` URLs (non-web schemes are ignored).

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

[Unreleased]: https://github.com/Helmi/tfd-bridge/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/Helmi/tfd-bridge/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/Helmi/tfd-bridge/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Helmi/tfd-bridge/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/Helmi/tfd-bridge/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Helmi/tfd-bridge/compare/v0.2.4...v0.3.0
[0.2.4]: https://github.com/Helmi/tfd-bridge/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/Helmi/tfd-bridge/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/Helmi/tfd-bridge/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/Helmi/tfd-bridge/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Helmi/tfd-bridge/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/Helmi/tfd-bridge/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Helmi/tfd-bridge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Helmi/tfd-bridge/releases/tag/v0.1.0
