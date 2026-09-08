# Shared replay and video layout

Accepted layout: 2026-09-08 (0.18.0, variant09). The interactive Replayer, layout preview and MP4
export now use the same `BroadcastFrame` component. `ResponsiveBroadcast` fits
that frame into the interactive player; `SharedFrameRenderer` captures it at
explicit replay timestamps for offline video encoding.

## Composition

- 1920×1080 frame: square tactical map on the left, owner statistics, compact
  ribbons, one chronological kill/chat feed and two rosters on the right.
- Scores and stable team-class icons overlay the map on a translucent surface.
  Team/roster class icons point right; dead ships dim in place. Map markers use
  recorded hull heading.
- Ship silhouette and ribbon images come from the local game through the scene
  exporter. No generated replacement artwork. Usernames appear under roster
  ship names. Division letters and colors use the same helpers as the Replayer.
- Map markers use the accepted 1.2× scale; map names are 13px uppercase JetBrains
  Mono with tight tracking. Roster ship names are 16px with 44px rows.
- No redundant section headings or bottom legend. Ribbons have corner count
  badges and appear only after their first timestamped count.
- The balanced header is 220px tall, with 36px damage and equal 18px current/max
  health text. Ship class words beside the selected ship are omitted.
- The 156px feed viewport contains natural-height rows: 15px ship/body text,
  14px soft-gray player identities, 11px timestamps and 16px event/class icons.
  Both chat and sinking show a game's right-facing class icon, affiliation-coloured
  ship name, then ` | [CLAN] Player` in #b8c2bc. Missing identities/classes are not
  invented. Chat resolves sender IDs first, with unique exact-name fallback for
  older scenes; message bodies retain their channel colour. Lucide ChevronRight
  marks chat; a red Lucide X marks sinking. No achievements are added to the feed.
- The entire feed moves by each arriving row's measured height over 3.2 replay
  seconds (320ms at 10×), including wrapped content. Movement follows the replay
  clock through pause, seek and overlapping arrivals. Reduced-motion preference
  disables movement. Living roster player names use the same soft gray; sunk
  rows retain their original dimming.
- Own-division ships (including the recorder in a valid division) have a small dot
  before their map name. The label and dot match the resolved icon colour and
  retain label visibility alpha. They stay centred and edge-clamped as one group,
  with a slightly smaller icon-to-label gap. Ship glyphs have no dot. Font sizes
  and roster remain unchanged.

## Data and preview

Owner damage, potential damage, spotting damage and ribbons now use sparse,
absolute `tracks.ownerStats` samples from wows-toolkit. They are not illustrative
counters. Older scenes without those tracks cannot supply those statistics.

Run the Vite development server and open `/broadcast-layout.html`. The preview
uses `public/generated/timeline-scene.json`; its transport sits outside the video
frame. The normal development Replayer can still fall back to a synthetic scene.
Bridge builds start with the local replay picker and use the native scene API.

Fonts are bundled locally with their licenses. Game images used by a scene are
extracted from the user's game installation. The old `layout-assets` directory
contains the initial design-review extracts and is not the current renderer's
source of ribbon or silhouette data.

## Rendering and delivery

Local H.264 MP4 encoding, file saving and background render jobs are implemented.
A real 763.765-second battle has been encoded to a 76.4-second 1080p30 MP4 below
20 MiB in browser validation. That cap is a test value: sharing must use the byte
limit supplied by Engine and validate the actual encoded file size.

The Replayer and Engine post-battle screen use the native sharing coordinator
for authorization, local rendering, upload and delivery-status polling. See
[video-sharing.md](../../../docs/video-sharing.md) for the implemented contract
and validation boundaries. Engine owns Discord delivery and temporary-object
cleanup. Live Discord posting still needs end-to-end verification.

Map consumables use original game icons bundled in `public/assets/consumables` (25 unchanged PNGs; manifest records source build and hashes). Radar/hydro ranges are resolved by the pinned toolkit in scene-export, and drawn with thin outlines and 1.2% fill behind markers. Hydro's dashed inner line denotes torpedo detection; the solid line denotes ship detection. Packet timing drives both map presentations, including simultaneous effects and seeks. Hidden/last-known/dead ships do not display active effects. No fighter patrol circle is inferred from an activation.
