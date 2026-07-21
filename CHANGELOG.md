# Changelog

All notable changes to TFD Bridge are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [SemVer](https://semver.org/).

---

## [Unreleased]

---

## [0.15.0] — 2026-07-21

### Fixed
- **Post-battle credit totals can now match the player's actual account result.** The replay API now includes whether the recording player used Warships Premium, allowing the Battle Monitor to apply the correct Premium multiplier and active economic bonuses before subtracting battle costs. Previously the API did not expose whether Premium was active and described the base credit amount as the final amount, which could make the detailed result image show an incorrect — sometimes negative — net total. The battle-result API schema is now 1.8.

---

## [0.14.0] — 2026-07-17

### Added
- **RePlayer** — the in-app replay viewer is now a real, visible tool, reached from the **RePlayer** button in the title bar (next to Dashboard and Battle Monitor). It decodes your saved battles locally and plays them back: a tactical map with ship tracks, torpedoes, capture zones, live score, the full roster, and a per-ship panel with health, consumables, damage taken and battle chat — all scrubbable along a timeline. (This was the "hidden test feature" from 0.12; it now has a home in the UI.)
  - Replay picker shows your newest 30 battles with a **Load 30 more** button, search, and a proper loading state; a decoding overlay appears while a battle opens (decoding a replay takes a few seconds).
  - Enemy torpedoes are drawn in enemy red so incoming fish read as a threat.
- **Launch on login now defaults to on** for new installs (existing installs keep whatever you already had; you can still turn it off in Settings).

### Changed
- **Replay donation is now on for everyone** — the opt-in/out toggle has been removed. Your `.wowsreplay` files upload automatically to power community stats and challenges; uploads are anonymous and can still be paused centrally by TFD.
- RePlayer now opens **inside the main window** (like the embedded Battle Monitor) instead of a separate window, so the title-bar navigation stays with you.
- Refreshed the RePlayer's look to the TFD Engine style (deep green-black, teal and gold).
- Updated the bundled wows-toolkit replay-decoding libraries to the current upstream — routine maintenance, with no change to the decoded battle data (re-validated against real replays).

### Known issues
- RePlayer's **"Incomplete" marker** — which flags a battle you left before it ended — may not flag some replays on the current game patch. Detection is confirmed working on earlier patches; a fix is planned.

---

## [0.12.1] — 2026-07-13

### Changed
- Improvements to the hidden test feature.

---

## [0.12.0] — 2026-07-12

### Added
- A hidden test feature.

---

## [0.11.0] — 2026-07-11

### Added
- Objective points in the post-battle data (decode schema 1.7): a per-player `victory_points` map with WG's full objective-points breakdown — cap captures, cap holds, cap defense, blocking, kills by ship class, victory bonuses, arms-race pickups, convoy escort — including the negative entry for losing your ship. All players, raw game field names.
- Ship and commander loadouts (decode schema 1.7): a per-player `build` object read from the battle-start packets — mounted modules, upgrades, consumables, signals/camouflage, ensigns, economic boosters, the commander's identity, the commander's learned skills for the ship's class, and the commander points spent. Present for every player in the battle, enemies included (a replay records all loadouts). All values are raw game ids; the web app translates them to names. Commander skills can be missing for an enemy ship the recording client never got close to — everything else is always populated.

---

## [0.10.1] — 2026-07-09

### Fixed
- Post-battle data now includes the full set of ribbons each player earned (decode schema 1.6): a per-player `ribbons` map keyed by the game's own ribbon names. Previously only a curated subset was exposed, so ribbons like bomb hits, assists, captures, crits, fires, floods, rocket hits and depth-charge hits were missing from the API — which is why the Battle Monitor's Discord share cards could not show them.

---

## [0.10.0] — 2026-07-08

### Added
- Main-battery hit quality in the post-battle data (decode schema 1.5): per-player penetration, over-penetration, non-penetration, ricochet and citadel counts, plus secondary-battery hits and torpedo-protection hits — for all players.
- Per-battle Ship Efficiency grade for the recording player (Expert / Grade I / II / III), so the Battle Monitor can show the efficiency badge.
- The recording player's active economic bonuses that battle — consumable boosters and permanent ship/commander bonuses — each with its category and multiplier, so the Battle Monitor can show which were in use.

### Changed
- Refreshed the bundled game-data index (`constants.json`) to the current WoWS build. The old index had drifted, which made the main-battery hit-quality ribbon fields read as zero; the refresh restores them and is why they can be exposed now.

---

## [0.9.0] — 2026-07-08

### Added
- Per-battle achievements in the post-battle data (decode schema 1.4): the medals each player earned that battle, so the Battle Monitor can show them.

### Fixed
- Corrected a damage double-count for aircraft carriers and airstrike ships: carrier-aircraft and airstrike bomb/depth-charge hits were each counted twice, inflating those players' damage totals and the per-weapon breakdown in the Battle Monitor. Per-player damage now reconciles with the game's own total.

---

## [0.8.0] — 2026-07-05

### Added
- Single-instance guard: launching TFD Bridge while it is already running now focuses the existing window instead of opening a second copy.
- More post-battle data for the Battle Monitor (decode schema 1.3): main-battery damage split by shell type (HE/AP/SAP), and the owning player's battle economics — service, ammunition, and consumable credit costs plus the premium multipliers.

---

## [0.7.2] — 2026-06-29

### Fixed
- Removed a phantom scrollbar on the embedded pages — full-height page layouts now fit below the title bar instead of overflowing by its height.

---

## [0.7.1] — 2026-06-29

### Fixed
- Unified title bar: one consistent bar on every view (dashboard and engine pages) — same height, logo, version, and nav.
- Settings no longer opens a black window when the app starts on the Battle Monitor.
- Post-battle stats: removed fields that showed wrong numbers (some ribbon counts, planes lost, capture points) and corrected the torpedo- and main-caliber-hit counts.

---

## [0.7.0] — 2026-06-29

### Added
- Unified title bar on every view — logo, version, the same nav (Back/Forward/Dashboard/Battle Monitor/Settings), and window controls.
- More post-battle stats for the Battle Monitor (decode schema 1.2): per-weapon damage, ribbons, detection, capture points, planes lost, module damage, structure damage, and team-XP share.

### Fixed
- Settings no longer opens a black window when the app starts on the Battle Monitor.

---

## [0.6.2] — 2026-06-24

### Added
- **Maximize/restore button in the Battle Monitor.** The embedded monitor's title bar now has the same maximize/restore control as the local Settings page — between minimize and close, with an icon and tooltip that reflect the current state. On Windows 10, double-clicking a frameless title bar does not maximize, so this button is the reliable way to maximize the monitor window (and any profile-link window opened with the **New Window** option).

### Fixed
- **Battle Monitor layout is correct on small / size-constrained windows.** The embedded page is now inset below the title bar as a single scroll area: content no longer starts *behind* the bar at the top or runs *under* the window's bottom edge, and there is exactly one scrollbar — it sits fully on-screen and reaches the very bottom. This holds on every engine page (live monitor, post-battle results, dashboard, profiles, clans), at any window height. (Replaces the earlier offset approach, which only shifted some of the engine's layout wrappers and left the page's outer container behind the bar.)
- **The engine's "Connect Discord" banner no longer hides behind the title bar.** For users who are signed in but have not linked Discord, the engine shows a connect-Discord bar at the very top of the page. It now sits directly below the app's title bar — fully visible, with page content below it — instead of being covered by the bar.

### Security
- The embedded engine pages' window-controls capability now additionally allows maximize/restore (`core:window:allow-toggle-maximize` / `allow-is-maximized`) — this is what powers the new maximize button. It remains window-manipulation only: the remote `engine.tfd.rocks` pages still have **no** file, bridge, store, dialog, or app access.

---

## [0.6.1] — 2026-06-23

### Fixed
- **Battle Monitor no longer shows a second scrollbar.** A regression in 0.6.0's title-bar offset left full-height pages (such as the post-battle results screen) a whole viewport tall inside the slightly shorter content area, producing a duplicate scrollbar. Full-height containers are now shrunk by the bar height (covering the dynamic-viewport units the engine uses), so there is a single scrollbar again.

---

## [0.6.0] — 2026-06-23

### Added
- **Choose where Battle Monitor links open.** A new Settings option — **"Profile links open in"** — controls what happens when you click a player, clan, or other engine link in the embedded Battle Monitor:
  - **Same window** — opens in place; the title bar keeps **Dashboard**, **Battle Monitor**, **Settings**, and back/forward controls, so you can open a profile and step straight back to the live monitor.
  - **New Window** — opens the link in its own in-app window, leaving the live monitor running untouched in the main window.
  - **Browser** — opens it in your default browser (the previous behaviour, and the default).

  Only `engine.tfd.rocks` links ever open in-app; any other site always opens in your browser.
- **Persistent navigation bar in the Battle Monitor.** The embedded monitor's title bar now always shows **Dashboard**, **Battle Monitor**, back/forward, and **Settings** — on every engine page — so you can navigate no matter where a link took you.

### Changed
- **The local TFD Bridge page is now "Settings".** What used to be called the "Dashboard" is really the settings page (replays folder, launch-on-login, replay donation, and the new link-target option). The title-bar **Dashboard** button now opens the **engine's** dashboard, and **Settings** opens the local page — clearing up the name clash between the two.
- **New installs: Replay donation is on by default (opt-out).** Previously every new user was asked first; a brand-new install now starts with donation enabled (shown on, with the full privacy copy, during onboarding — you can decline). **Existing installs are unchanged** — whatever you chose is kept, and anyone who never answered the original prompt still gets the one-time ask. As always: a replay contains the battle data of all players in the match, including their names and in-game chat; only `*.wowsreplay` files are uploaded; uploads are anonymous (no account); donation can be paused server-side; and anything already uploaded stays donated if you later opt out.

### Fixed
- **Battle Monitor content no longer hides under the title bar.** On the current engine layout the page's fixed header could overlap the app's title bar; the page is now offset correctly so nothing sits behind the bar.

---

## [0.5.1] — 2026-06-21

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

[Unreleased]: https://github.com/Helmi/tfd-bridge/compare/v0.15.0...HEAD
[0.15.0]: https://github.com/Helmi/tfd-bridge/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/Helmi/tfd-bridge/compare/v0.12.1...v0.14.0
[0.12.1]: https://github.com/Helmi/tfd-bridge/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/Helmi/tfd-bridge/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/Helmi/tfd-bridge/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/Helmi/tfd-bridge/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/Helmi/tfd-bridge/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/Helmi/tfd-bridge/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/Helmi/tfd-bridge/compare/v0.7.2...v0.8.0
[0.7.2]: https://github.com/Helmi/tfd-bridge/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/Helmi/tfd-bridge/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/Helmi/tfd-bridge/compare/v0.6.2...v0.7.0
[0.6.2]: https://github.com/Helmi/tfd-bridge/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/Helmi/tfd-bridge/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/Helmi/tfd-bridge/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/Helmi/tfd-bridge/compare/v0.5.0...v0.5.1
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
