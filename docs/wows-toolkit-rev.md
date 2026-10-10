# wows-toolkit dependency revision

TFD Bridge embeds **two** in-process WoWS replay decoders, both built on
[landaire/wows-toolkit](https://github.com/landaire/wows-toolkit) (MIT), pulled
through our fork **`Helmi/wows-toolkit`** as git dependencies:

| Decoder | Crate | What it produces |
| --- | --- | --- |
| Battle-result decoder | `crates/bridge-core` (`battle_result.rs`) | Post-battle results (RIBBON_* etc.) for the `/v1/replays/{name}/result` endpoint — the engine-facing decode schema. |
| Scene decoder | `crates/scene-export` | The self-contained battle-scene JSON the hidden replay player renders. |

## One rev for the whole workspace

Both crates live in the same Cargo workspace, so they share **one** `Cargo.lock`
and therefore **one** resolved copy of every wows-toolkit crate and its
transitive deps (`pickled`, `bevy_ecs`, …). They **must** be pinned to the same
`(git source, rev)`. Two different revs put two copies of `wowsunpack` /
`wows_replays` in the tree, which then force a single incompatible `pickled`
version onto one of them and fail to build.

**Current rev: `6e1973e3c225f2e95859bb7d35968f87d1345466`** — landaire main
`6e1973e3` (2026-10-09), mirrored as branch `tfd-bridge/upstream-6e1973e3` on
`Helmi/wows-toolkit`. Upstream force-pushes `main`, so every pin gets its own
`tfd-bridge/upstream-<shortsha>` branch in the fork; the fork then serves the
commit regardless of upstream reachability. (GitHub fork networks also serve
upstream commits by SHA, which is what Cargo resolves against until the branch
exists.) Previous rev `b6a52397` (branch `tfd-bridge/upstream-b6a52397`).

> **Upstream force-pushed `main`** (observed 2026-08-16:
> `f3283972...040548ef main -> origin/main (forced update)`; by 2026-09-30
> `b6a52397` shares **no** merge base with `f328397`). Anything pinning a
> pre-rewrite upstream SHA is on borrowed time — including
> `experiments/replay-web-player/exporter`, which still pins
> `landaire/wows-toolkit @ f328397` and is built by `release.yml` as the
> scene-exporter sidecar.

**Build requirements at this rev:**

- `rustc >= 1.97` (upstream `rust-version`, edition 2024). CI uses
  `dtolnay/rust-toolchain@stable`, so it follows; local toolchains older than
  that fail with a plain "requires rustc 1.97" error. `src-tauri`'s own
  `rust-version = "1.77.2"` is therefore stale metadata.
- **Windows: `git config --global core.longpaths true`.** Upstream vendors a
  Buck `prelude/` with paths far beyond `MAX_PATH`; without the setting Cargo's
  git checkout fails with `path too long: ...prelude/toolchains/android/...`
  (libgit2 honours `core.longpaths`, no registry change needed).

The fork's own `main` branch is intentionally stale and is **not** used — we pin
explicit SHAs. "Bumping the fork" means moving these SHAs to a newer upstream
commit; the fork is not pinned for any special reason and can track upstream.

## History

- Bumped to `6e1973e3` (landaire main, 2026-10-09) on 2026-10-10 — **WoWS 15.9 broke the
  RePlayer roster.** 15.9 inserted `shipFrags` into the player/bot vehicle
  data, shifting `shipId` / `shipParamsId` / `skinId` (and bots' `teamId`).
  At `b6a52397` the scene therefore read garbage team ids (21 "teams" instead
  of 2) and the RePlayer could not show 15.9 replays. Upstream `f71579a3`
  version-gates the 15.9 key maps and `58737bcf` lets per-build constants
  (`PLAYER_NUM_MEMBER_MAP` / `BOT_NUM_MEMBER_MAP`) override them. No code
  changes on our side; `constants.json` refreshed to 15.9.0_13357625 (result
  layouts unchanged). Re-validated: workspace compiles warning-free, all tests
  pass; 7 real 15.9 scenes have 2 teams / 24 named ships each, 15.8 scenes
  unchanged; 14/15 local 15.9 battle results decode `ok` (24/24 loadouts,
  ship ids consistent; the 15th left early), and the 15.8 economy e2e still
  reproduces the in-game screen exactly. Crate versions: `wows-battle-world` 0.14.0, `wows_minimap_renderer` 0.40.0, `wows_replays` 0.47.0, `wowsunpack` 0.46.0.

- Bumped to `b6a52397` (landaire main, 2026-09-30) on 2026-10-05 — catch-up to
  upstream after the history rewrite (no common ancestor with `d1c317e5`).
  `wows_replays` 0.44→0.46, `wowsunpack` 0.43→0.45, `wows-battle-world`
  0.11→0.13, `wows_minimap_renderer` 0.37→0.39. Code changes were mechanical:
  `ReplayFile::packet_data` is now a method (bridge-core ×2, scene-export ×4),
  `Degrees`/`Radians` lost their public `.0` (use `.value()` /
  `.to_degrees()`), and `visibility_flags()` returns `Option<VisibilityFlags>`
  instead of a raw `u32` (mapped to `is_some_and(|f| f.raw() != 0)`, same
  truth table). `Cargo.lock` needed targeted `cargo update -p` of `rustc-hash`,
  `tokio`, `toml`, `toml_datetime`, `proc-macro-crate`. Validation: workspace
  compiles with 0 warnings, `cargo test --workspace` 292 passed / 0 failed
  (4 gated-ignored). **Battle-result JSON is byte-identical** between the old
  and new build on 38 archived replays (21×15.8 + 3×15.7 decodes, schema 1.8,
  plus 14 no-BattleResults errors with identical messages). Scene export
  (`dump_scene`, 15.8 replay) still produces the full scene (24 ships, 857
  hits, same size); the only difference is `events.hits[].victimId` on 119 of
  857 hits (+6 derived `impactTargetGuessId`): the old rev could only resolve a
  victim through a matched salvo and fell back to the **owner's ship** when
  none matched, the new rev resolves the ship nearest the shell's own impact
  position — an upstream correctness fix, not a schema change.
- Bumped to `d1c317e` on 2026-08-16 — **WoWS 15.7 broke replay decoding.** 15.7
  introduced a `FLOAT64` type in the entity-definition specs; `parse_type` in
  `wowsunpack/src/rpc/typedefs.rs` had no branch for it, so every 15.7 replay
  fell through to a `panic!`. `catch_unwind` in `battle_result.rs` turned that
  into `"replay parser panicked (incomplete or unsupported replay)"` — the
  post-battle screen simply stopped appearing. Fixed by cherry-picking upstream
  `25d96db6` (a two-line mapping; `PrimitiveType::Float64` was already fully
  plumbed at our pin) onto `f328397` rather than bumping to upstream `main`.
  Deliberate: the ~30 decode-relevant commits since our pin include a game-data
  seam refactor, a typestate `ReplayFile`, and zero-copy metadata APIs, and
  `25d96db6` sits *after* all of it — so pinning to it would carry the same
  churn as jumping to the tip. Re-validated: workspace compiles with no code
  changes, all 256 tests pass (battle-result schema unchanged), and 8/8
  finished 15.7 replays decode with 24 players each. Ribbon sub-counts are
  internally consistent (`MAIN_CALIBER 38 = PENETRATION 22 + NO_PENETRATION
  16`), and `CLIENT_PUBLIC_RESULTS_INDICES` is identical between the bundled
  and installed `constants.json` (538 keys, 0 diffs) — so 15.7 needs **no**
  constants refresh.
- Bumped to `f328397` (landaire main, 2026-07-09) on 2026-07-17 — routine
  catch-up to upstream (the intervening commits are armor-viewer / camouflage /
  texture-rendering work, none touching replay decoding). Re-validated: workspace
  compiles, all 175 bridge-core tests pass (battle-result schema unchanged), and
  a real 15.6 scene decodes intact (24 ships, tracks, salvos/torpedoes/kills).
  `wows_replays` 0.43→0.44, `wowsunpack` 0.42→0.43; no schema change.
- The battle-result decoder shipped on `50301ee` (2026-06-06).
- The scene decoder (lifted from the standalone `experiments/.../exporter`) was
  written and validated against `36c4e41` (2026-06-29). Between those revs
  `wows-battle-world` gained the `scan` module (`scan_replay_world` /
  `WorldScanCollector`) the scene decoder drives, and `wows_replays`'
  `decoder/decode.rs` was substantially reworked.
- Moving the scene decoder *back* to `50301ee` was not viable (no `scan` API).
  Per the owner's direction, both decoders were **unified forward onto the newest
  rev that supports both = `36c4e41`**, and each decode was re-validated (see
  below). This is a maintenance decision, not a schema change by intent.

## When bumping the rev

Change the `rev` in **both** `crates/bridge-core/Cargo.toml` and
`crates/scene-export/Cargo.toml` to the same SHA, then:

1. `cargo check --workspace` — both decoders must compile.
2. **Re-validate the battle-result decode**: decode a known replay via the
   `/result` path and confirm the output schema is unchanged. A change here is
   an engine-facing schema change — coordinate with the engine (Scotty) first.
3. **Re-validate the scene decode**: open a real replay in the player and
   confirm it renders (map, ships, tracks).

If a newer rev breaks either decoder, step back to the newest rev that builds
and passes both validations.
