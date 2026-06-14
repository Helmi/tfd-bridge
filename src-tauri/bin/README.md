# Bundled sidecar binaries

## replayshark-x86_64-pc-windows-msvc.exe

The [landaire/wows-toolkit](https://github.com/landaire/wows-toolkit) `replayshark`
CLI (MIT), used by the bridge to dump a `.wowsreplay` packet stream and extract the
final `BattleResults` packet (see `crates/bridge-core/src/battle_result.rs`).

- **Source:** `~/code/wows-toolkit` `target/release/replayshark.exe`
- **Pinned upstream commit:** `50301ee54630f38d7e8014d98e50e833e15fbea6`
- **Supported WoWS replay versions:** 15.3 / 15.4 (per `game_versions.toml` at that commit)
- **Target triple:** `x86_64-pc-windows-msvc` (Tauri `externalBin` naming convention)

### Refreshing for a new game patch

When WoWS ships a new version, rebuild the toolkit and re-copy:

```sh
cd ~/code/wows-toolkit && git pull && cargo build --release -p replayshark
cp target/release/replayshark.exe \
   <repo>/src-tauri/bin/replayshark-x86_64-pc-windows-msvc.exe
```

Then refresh `../resources/constants.json` and `../resources/ship_index.json` (see
`../resources/README.md`) and bump the commit hash above. The `#[ignore]`d
`TFD_DECODE_E2E` integration test in `bridge-core` is the canary that the new
binary still resolves results correctly.
