# Battle Scene v1 (experimental)

`tfd-replay-scene` is a renderer-neutral transport model. Times are integer
milliseconds since the battle phase began. Map coordinates are normalized to
the top-left-origin `[0, 1]` tactical surface.

## Static data

- Replay identity, source SHA-256, game build, duration, perspective.
- Map identity, projection size, optional image reference.
- Entity/roster dictionary: player, clan, ship, species, team, relation, max HP.

## Continuous and stepwise tracks

Ship samples contain:

- `x`, `y`: interpolated continuous position.
- `headingDeg`: compass heading (`0` north/up, `90` east/right); interpolate by
  the shortest angular path.
- `hp`, `maxHp`, `alive`: stepwise damage state.
- `visible`: currently observed by the recording perspective.
- `lastKnown`: the position is stale because the target is no longer observed.
- `detectedByEnemy`: replay detection flags indicate the ship is spotted by the
  opposing side. This is distinct from `visible`.
- `submerged`: vehicle state reports an invisible/submerged state.

Scores and capture point values are step functions. The viewer holds the last
sample at or before the requested time.

Arms Race pickup zones use the same stepped-state principle but remain a
separate semantic type from capture points. Each buff sample carries its zone
entity ID, position, radius, active state, optional team, and the opaque game
`markerName` resolved from its Drop parameter. A zone is inactive before its
first sample and again when its entity leaves after collection. `activationAt`
preserves the Drop record's authoritative `startTime`, allowing the renderer to
distinguish a visible waiting zone from a collectible one. Scene assets may map
each marker name to the matching inactive and active game icons.

## Events and lifecycles

- Artillery salvo: fire time plus per-shell world-projected origin, target, and
  expected flight time. The viewer derives tracer position at time `t`.
- Torpedo: launch/update samples plus end time. The viewer interpolates or
  extrapolates between authoritative updates.
- Kills/deaths: timestamped killer, victim, and decoded cause.

Since 2026-07-12 the exporter also emits:

- `tracks.smoke`: per smoke-screen entity, the accumulating puff cloud
  (positions + one shared puff radius) with a stepped active flag; the whole
  cloud goes inactive at its dissipation `EntityLeave`.
- `tracks.planes` + scene-level `aviation` descriptors: sparse squadron
  position samples keyed by a generation-unique id (game plane ids are reused
  after removal), with owner, team, species kind (fighter/bomber/dive/scout),
  category (controllable/consumable/airsupport), and `iconDir`/`iconBase`
  resolved the same way the minimap renderer resolves squadron markers. The
  actual game icon PNGs (own/ally/enemy variants) are exported to
  `assets.planeIcons` so the player draws the real squadron markers.
- `events.salvos[].ammoType`: `AP` | `HE` | `SAP` per salvo, resolved from the
  projectile param, so the player can color shell tracers by shell type.
- `events.hits`: resolved main-battery shell hits — `{t, attackerId, victimId,
  ammoType?, quality}` where quality is penetration/citadel/overpen/shatter/
  ricochet/underwater. The player attributes each HP loss by matching a drop to
  penetrating hits within a tight window (attacker + shell type + quality),
  labels recurring equal ticks as fire, and leaves the rest unattributed —
  honest to the single perspective, which often never sees the enemy torpedo
  or fire-starter.
- `wards`: stationary fighter-patrol circles as lifecycle records
  (`addedAt`/`removedAt`).
- `events.consumables`: every observed activation (ship, game consumable
  name, duration).
- `events.chat`: full battle chat (clock, sender, division/team/global/system
  channel, message). Countdown banter is clamped to `t = 0`, not dropped.
- `events.pickups`: Arms Race collections attributed to the collecting ship.
  The `drop.picked` message carries no zone id, so the exporter matches the
  pick to the nearest active zone with the same Drop param and ends that zone
  at the exact pickup time (superseding the ~1.25 s EntityLeave lag).

## Perspective honesty

Every scene declares its perspective. A single client replay only contains what
that client received. In particular:

- Enemy positions normally exist while detected and then as last-known state.
- Enemy HP can be stale while the enemy is outside observation.
- Damage involving unseen ships can be absent.
- Builds and chat are perspective-dependent.

The player must display stale/unknown state honestly rather than imply
omniscience. A future merged scene should list every contributing replay/team.
