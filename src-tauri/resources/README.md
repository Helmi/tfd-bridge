# Bundled decode resources

These are read at runtime by the battle-result decoder
(`crates/bridge-core/src/battle_result.rs`) to resolve the positional arrays in a
replay's `BattleResults` packet into named fields, and to map ship ids to
tier/class.

## constants.json

The [landaire/wows-toolkit](https://github.com/landaire/wows-toolkit)
`embedded_resources/constants.json` (MIT). Provides `CLIENT_PUBLIC_RESULTS_INDICES`,
`CLIENT_VEH_INTERACTION_DETAILS`, `COMMON_RESULTS`, and `PLAYER_PRIVATE_RESULTS`.

- **Pinned upstream commit:** `50301ee54630f38d7e8014d98e50e833e15fbea6`

## ship_index.json

`vehicle_id → { index, name, level, species, nation, group }`, distilled from the
game's `GameParams` via `wowsunpack game-params --full` (see
`private-sync/notes/xp-analysis/build_ship_index.py`). Used for ship name / tier /
class enrichment. Unknown ids (ships added in a newer patch) degrade gracefully to
`null` tier/class.

## Refreshing for a new game patch

```sh
cp ~/code/wows-toolkit/embedded_resources/constants.json ./constants.json
# regenerate ship_index.json from the current game build, then:
cp <new ship_index.json> ./ship_index.json
```

Keep the pinned commit in sync with `../bin/README.md`.
