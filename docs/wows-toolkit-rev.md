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

**Current rev: `d1c317e5e10b9e674fb352b159fe81c9cc6e652e`** — branch
`tfd-bridge/float64-15.7` on `Helmi/wows-toolkit`. This is landaire main
`f328397` (2026-07-09) plus a single cherry-pick of upstream `25d96db6`
(FLOAT64 entity-spec support, see below). Earlier revs were served by
`Helmi/wows-toolkit` by SHA (GitHub fork networks serve upstream commits by
SHA); this one lives on a real branch in the fork, so it does not depend on
upstream reachability.

> **Upstream force-pushed `main`** (observed 2026-08-16:
> `f3283972...040548ef main -> origin/main (forced update)`). Our old pin
> `f328397` is no longer an ancestor of upstream `main`. It still resolves by
> SHA today, but anything pinning a pre-rewrite upstream SHA is on borrowed
> time — including `experiments/replay-web-player/exporter`, which still pins
> `landaire/wows-toolkit @ f328397`.

The fork's own `main` branch is intentionally stale and is **not** used — we pin
explicit SHAs. "Bumping the fork" means moving these SHAs to a newer upstream
commit; the fork is not pinned for any special reason and can track upstream.

## History

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
