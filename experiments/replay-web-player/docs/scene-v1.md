# Battle Scene v1 (experimental)

`tfd-replay-scene` is a renderer-neutral transport model. Times are integer
milliseconds since the battle phase began. Map coordinates are normalized to
the top-left-origin `[0, 1]` tactical surface.

## Static data

- Replay identity, source SHA-256, game build, duration, perspective.
- Optional `replay.outcome`: `victory`, `defeat`, or `draw`, relative to the recording player. Scene export uses wows-battle-world's winning team; absent means unknown, never an inferred loss. Playback duration remains battle-only. The shared player/video presentation adds a 4.5-second ending in screen time (3-second result hold, 1-second fade, 0.5-second black hold).
- Map identity, projection size, optional image reference.
- Entity/roster dictionary: player, clan, ship, species, team, relation, max HP.
- Optional entity `tier` is a JSON number from 1 through 11 (superships use 11),
  read from the toolkit ship parameter's vehicle `level()`. Missing vehicle
  metadata and values outside this range omit the field. Consumers must also
  accept older scenes without it; ship codes and names are not tier sources.
- Optional entity `divisionId` (string, preserving the full integer ID) and
  `divisionLabel` (A, B, C...). Membership comes from wows-toolkit. Labels are
  reconstructed across both teams in first-appearance order in the initial
  arena player roster, decoded with the toolkit. Sorting IDs and the pinned
  toolkit's per-team labels both disagree with the supplied screenshots.
  Arena order matches all divisions in the 2026-09-04 Los Andes, Lenin, and
  Bismarck '41 replays and the 2026-09-03 Libertad replay. The recording
  player's division appears first (A) in all four; a universal
  perspective-first rule is not independently verified.
  Membership comparisons use team and ID, never the label. Older scenes may
  omit both fields. The recording player's division, including the recorder,
  gets yellow ship icons. Selection overrides the icon color with pink instead
  of adding a white ring; labels and health indicators retain their normal colors.

## Continuous and stepwise tracks

`tracks.ownerStats` contains sparse absolute snapshots of the recording player's
`damage`, `potentialDamage`, `spottingDamage`, and `ribbons` (translation key to
count). The toolkit's current battle view supplies these values; they are not
final results copied into the opening frame. Hold each snapshot until the next
one, including on backward seeks. Before the first snapshot, or for older scenes
without this track, statistics are unavailable. An observed empty ribbon map means
no ribbons have been earned yet.

`assets.ribbons` maps the same keys to `label`, `iconKey`, `isSubribbon`, and an
optional inline `imageUrl` loaded from the game. Only icons used by the replay
are embedded. `assets.ownerSilhouette` optionally contains the recording ship's
original PNG silhouette. Both assets are consumed by the shared HTML/video frame.

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
  flight time. Optional projectile `path` contains absolute scene-time samples
  `{t, x, y}` from toolkit-derived RK4 ballistics (projectile mass, caliber,
  drag, muzzle velocity, replay pitch, and the toolkit's 2.75 game-time scale).
  Horizontal progress therefore changes with velocity rather than moving at a
  constant speed. The pinned toolkit's GUI-only integrator is adapted in
  `crates/scene-export/src/ballistics.rs`, with its MIT license alongside it.
  Matched replay impacts reconcile each shell's endpoint and arrival time while
  preserving its ballistic progress curve. The first impact ends the tracer;
  subsequent exit/ricochet records do not extend it. Without ballistic data,
  the exporter falls back to time-scaled server flight time and a straight path.
  Older scenes without `path` retain the existing linear interpolation.
  `impactRecorded` distinguishes reconciled collisions from predicted endpoints.
  Optional `impactTargetGuessId` is diagnostic only: the pinned toolkit estimates
  the victim from the salvo's aim location and may get it wrong. Never use this
  guess to snap an impact onto a ship, or to turn a miss into a hit.
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
  RePlayer derives RPF sectors from complete compass-range messages such as
  `RPF: ESE~SE`, retaining the latest report per sender at the replay clock.
  The sector follows the sender's displayed position, disappears when hidden
  or sunk, and remains in the chat feed. It adds no inferred enemy position.
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

### Observed consumable visualization (0.18 local iteration)

Each `events.consumables[]` may include optional `visual` metadata:

- `iconKey`: game PCY ability name; frontend uses bundled original artwork with a supported-type fallback.
- `abilityVariant`: matched game ability category when unambiguous.
- `source`: `equipped-ability`, `ship-definition`, `ship-range-fallback`, or `ambiguous-ability`. All metadata comes from the replay build's toolkit GameParams. Missing equipment does not become claimed equipped data.
- `shipRangeMeters`, `torpedoRangeMeters`: distinct detection ranges. These do not reveal enemy entities or imply all targets in the circle are observed.
- `shipRadius`, `torpedoRadius`: fraction of full map width, converted with toolkit unit types and MapInfo. These are NOT raw BigWorld units or metres. Radii are omitted when missing, invalid or ambiguous; other supported observations remain icon-only.

Timing remains `t` + packet-supplied `durationMs`; no config duration replaces it. Radar/hydro rendering skips dead, hidden and last-known ships. Solid circles indicate ship detection; the faint dashed hydro inner circle indicates torpedo detection. When hydro ranges coincide, one boundary represents both. Replayer and video use the same layer, clipped to the map behind ship markers.

Aircraft descriptors now obtain `ownerId` through toolkit `PlaneId.owner_id()`, not the packet-recipient field. Existing observed aircraft tracks and ward geometry remain authoritative. Fighter/spotter activation alone never synthesizes a patrol/detection circle. Unknown consumable types remain unvisualized.
