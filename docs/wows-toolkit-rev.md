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

**Current rev: `36c4e41a6366115f3ddfc8355ca32d328e81625c`** (landaire main,
2026-06-29), served by `Helmi/wows-toolkit` by SHA (GitHub fork networks serve
upstream commits by SHA, the same way the previous `50301ee` pin resolved).

The fork's own `main` branch is intentionally stale and is **not** used — we pin
explicit SHAs. "Bumping the fork" means moving these SHAs to a newer upstream
commit; the fork is not pinned for any special reason and can track upstream.

## History

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
