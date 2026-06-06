# TFD Bridge

Local desktop companion for the [TFD](https://engine.tfd.rocks) *World of Warships* community. It runs quietly in your system tray and bridges local game files to the TFD web app over loopback HTTP — so features that need your `replays/` folder work even when the browser can't reach it.

> Source is open here; **Windows installers are published on the [Releases page](https://github.com/Helmi/tfd-bridge/releases)**.

## Why it exists

[Battle Monitor](https://engine.tfd.rocks/monitor) needs your World of Warships `replays/` folder. Browsers can't read it in two common cases:

- Steam installs live under `C:\Program Files (x86)\…`, where Chromium blocks the File System Access API.
- Brave disables the directory picker by default.

TFD Bridge reads the folder as a normal local process and serves it **read-only** over `http://127.0.0.1`, which the HTTPS web app can fetch (loopback is a trustworthy context, so it isn't mixed-content-blocked).

## Install (Windows)

1. Download the latest `TFD.Bridge_x.y.z_x64-setup.exe` from [Releases](https://github.com/Helmi/tfd-bridge/releases).
2. Run it. The app is **not code-signed yet**, so Windows SmartScreen shows a warning — click **More info → Run anyway**.
3. On first start, confirm your WoWS replays folder (auto-detected for Steam / Wargaming Game Center, or pick it manually).

The app then lives in the tray. **Left-click the tray icon** to open Battle Monitor; right-click for the menu. It keeps itself up to date automatically.

## What it does

- Detects your replays folder and serves it read-only on `127.0.0.1:43210` (falls back to `43211`–`43214`).
- Watches the folder and keeps the listing fresh; serves live battles (`tempArenaInfo.json`) and archived replays in nested subfolders.
- Optional **Launch on login**, set during onboarding or from the tray.
- Can embed Battle Monitor in-app for browsers that can't run it.
- Auto-updates from signed GitHub releases.

## Security & privacy

- Binds **loopback only** (`127.0.0.1`), **read-only**, with strict CORS to `https://engine.tfd.rocks`.
- Serves only replay files (`*.wowsreplay`, `tempArenaInfo.json`); path traversal and symlink escapes are blocked.
- No telemetry, no account — nothing leaves your machine except what the TFD web app fetches from loopback.
- Update artifacts are ed25519-signed.

## Build from source

Requires Rust and the [Tauri v2](https://v2.tauri.app) prerequisites.

```sh
cargo build                # build the workspace
cargo test                 # run the test suite
bunx @tauri-apps/cli dev   # run the app locally
```

Layout: the bridge is `crates/bridge-core`, the desktop app is `src-tauri`, and the UI is static HTML/CSS in `ui/`.

## License

[MIT](LICENSE) © Helmi
