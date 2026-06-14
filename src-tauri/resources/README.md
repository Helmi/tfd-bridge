# Bundled decode resources

These JSON data files are bundled with the app (`tauri.conf.json` →
`bundle.resources`) and read at runtime by the in-process battle-result decoder
(`crates/bridge-core/src/battle_result.rs`). The replay **parser** itself is the
`wows_replays` library crate (a pinned git dependency on a fork of
landaire/wows-toolkit — see `crates/bridge-core/Cargo.toml`); there is no bundled
executable. These files only drive the *resolution* of the decoded
`BattleResults` blob into named fields.

## constants.json

The [landaire/wows-toolkit](https://github.com/landaire/wows-toolkit)
`embedded_resources/constants.json` (MIT). Provides `CLIENT_PUBLIC_RESULTS_INDICES`,
`CLIENT_VEH_INTERACTION_DETAILS`, `COMMON_RESULTS`, `PLAYER_PRIVATE_RESULTS`, and
`INIT_ECONOMICS_INDICES` — the positional-array → named-field maps.

## ship_index.json

`vehicle_id → { index, name, level, species, nation, group }`, distilled from the
game's `GameParams` via `wowsunpack game-params --full` (see
`private-sync/notes/xp-analysis/build_ship_index.py`). Used for ship name / tier /
class enrichment. Unknown ids (ships added in a newer patch) degrade gracefully to
`null` tier/class.

## Refreshing for a new game patch

Re-copy `constants.json` from the upstream toolkit and regenerate
`ship_index.json` from the current game build:

```sh
cp <wows-toolkit>/embedded_resources/constants.json ./constants.json
# regenerate ship_index.json from the current game build, then:
cp <new ship_index.json> ./ship_index.json
```

When bumping the `wows_replays` parser dependency for a new game version, update
the pinned `rev` in `crates/bridge-core/Cargo.toml` to match.
