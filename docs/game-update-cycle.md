# Game update cycle

Run this for every World of Warships patch and hotfix, after the client is updated on the Windows PC.

## Command

```sh
uv run --with ijson --with httpx python scripts/update-game-data.py
```

Add `--dry-run` to write nothing into the repo. Add `--keep-dump` to keep the GameParams dump (path is printed).
Defaults for game dir, wowsunpack and the wows-toolkit checkout can be changed with `--game`, `--wowsunpack`, `--toolkit`, `--repo` (see `--help`).

The script:

1. Reads the client version and build from `<game>\game_info.xml`.
2. Dumps GameParams with `wowsunpack` into a temp dir (about 3 GB, 1-2 minutes) and deletes it afterwards.
3. Rebuilds `ship_index.json`, `achievement_index.json` and `bonus_index.json` in `src-tauri/resources/` (`scripts/build_indexes.py`, streaming, existing ship order kept).
4. Downloads the build's `constants.json` from `landaire/wows-replay-data` and writes it to `src-tauri/resources/`. If the build is not published yet, it says so and continues. Run it again in a few days.
5. Prints a summary: new and removed ships, achievements and bonuses; changed constants keys (loud warning if a decode-critical key changed); `KNOWN_GOOD` status; upstream wows-toolkit commits since our pin.

It never commits, pushes, or edits Rust/TS code. When nothing changed in the game data, `git status` stays clean.

## Manual follow-ups

1. Review the diff of `src-tauri/resources/*.json` and commit it.
2. Constants warning? Re-check the battle-result decoder before shipping.
3. Bump the wows-toolkit pin in `crates/bridge-core/Cargo.toml` and `crates/scene-export/Cargo.toml` (one rev for both). Update `docs/wows-toolkit-rev.md`.
4. Add the new `<major>.<minor>` to `KNOWN_GOOD` in `crates/bridge-core/src/battle_result.rs`, after a decode sweep on replays recorded with the new client.
5. Refresh the Dropbox `wows-assets` subset.
6. Tell the Engine and recorder agents (Dropbox `.agents/`).
7. Update `WANT_VERSIONS` in the private xp-analysis pipeline (`private-sync/notes/xp-analysis/`). That pipeline is not part of this script.

## What updates itself at runtime

Ship and map display names are read from the installed client's catalog. They need no refresh.
