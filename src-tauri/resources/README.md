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
`scripts/build_indexes.py`). Used for ship name / tier /
class enrichment. Unknown ids (ships added in a newer patch) degrade gracefully to
`null` tier/class.

## achievement_index.json

`achievement_id → WG name` (flat map, e.g. `"4281525168": "PCH012_Arsonist"`),
distilled from the game's `GameParams` (`typeinfo.type == "Achievement"` entries)
via `scripts/build_indexes.py`. Resolves the
per-player `achievements` public field (a list of `[id, count]` pairs) to a
name for schema 1.4. `name` is the entry's own dict key / `name` field — WG's
closest thing to a stable, human-decipherable achievement identifier (there is
no short "index" code like ships have; the `PCH###` prefix alone is
meaningless without the descriptive suffix). Unknown ids (achievements added in
a newer patch) fall back to the stringified integer id — nothing is lost.

## bonus_index.json

`economic-bonus id → { index, kind, modifiers, name, rarity }` for the economic
boosters (`kind: "booster"`, GameParams species `Camoboost`, `PCEA*`) and
permanent ship/commander bonuses (`kind: "permanent"`, species `Multiboost`,
`PCEU*`), distilled from the game's `GameParams` via
`scripts/build_indexes.py` (booster rules in `scripts/build_bonus_index.py`). Resolves the numeric ids
in the owner's private `subtotal_economics` chains (`economic_bonuses`,
`economic_breakdown`). An id missing here still appears in
`economic_breakdown` as `kind: "unknown"` with its applied factor.

## Refreshing for a new game patch

Run one command (dumps GameParams, rebuilds the three indexes, fetches `constants.json`, prints a summary):

```sh
uv run --with ijson --with httpx python scripts/update-game-data.py
```

See `docs/game-update-cycle.md` for options and the manual follow-ups. The builder lives in
`scripts/build_indexes.py` (booster rules in `scripts/build_bonus_index.py`).

When bumping the `wows_replays` parser dependency for a new game version, update
the pinned `rev` in `crates/bridge-core/Cargo.toml` and `crates/scene-export/Cargo.toml`
(one rev for both; see `docs/wows-toolkit-rev.md`).
