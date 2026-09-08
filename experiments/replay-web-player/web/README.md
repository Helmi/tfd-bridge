# Replay Web Player experiment

The browser frontend for TFD Bridge's World of Warships Replayer and local video
renderer. The directory retains its original experiment name.

The important boundary is the `ReplaySceneV1` transport contract and `loadReplayScene()` adapter in `src/types.ts` / `src/engine/importScene.ts`: decoded replay semantics go in, while presentation choices stay in the viewer. The deterministic synthetic battle remains a fallback UI/test harness.

The tactical layer uses PixiJS 8 with the WebGL renderer explicitly requested. React/DOM owns controls, rosters, readable stats, and accessibility. This lets us change the tactical renderer later without changing the exported scene data. On startup the app tries `/generated/scene.json`; its relative map asset is resolved against that JSON URL and drawn behind the tactical grid. If it is absent or invalid, the synthetic scene loads instead.

```powershell
npm install
npm run dev
npm test
npm run build
```

Current prototype semantics:

- shortest-arc interpolation for ship yaw and linear position/course interpolation;
- step tracks for health, visibility, team score, and cap progress;
- no enemy pose before its first observation, plus fixed last-known positions;
- capture owner, invader, normalized progress, and blocked-state rendering;
- viewpoint-aware `spotted`, `last-known`, and `hidden` ship knowledge;
- class-specific WoWS minimap glyphs, ship-name labels, authoritative hull yaw,
  and a nondirectional detection halo;
- time-bounded shell and torpedo trajectories;
- discrete damage events and selected-ship detail.

During development, **Choose replay** lists the local WoWS replay folder and
prepares the selected battle through the native exporter. This Vite middleware
is development scaffolding. Bridge builds instead use the native loopback replay
list and scene endpoints, plus the narrow local render-job routes below.

The decoder should emit this shape (or an evolution of it) in time chunks. It should not emit PixiJS commands. WoWS track coordinates are currently normalized `0..1`; `map.spaceSize` is retained as metadata, not used as viewer bounds. Speed is shown as unknown unless the exporter supplies a trustworthy `speedKnots` value.

## Shared layout and local video jobs (September 2026)

The Bridge build now uses the same `BroadcastFrame` as offline MP4 export.
The enemy team's top icon strip is mirrored; both side rosters retain normal
class/tier order. Bees to Honey, Two Brothers, Ice Islands and Shards use bundled
land/water artwork in both views. The original map hash guards each replacement,
and the final alpha mask retains the original geography. Prompts, source assets
and the reproducible finishing workflow are in [map-art](../map-art/README.md).
Timestamped owner statistics, original ribbon assets, ship silhouettes and
shared division colors are part of the native scene contract. Recorded shell
collisions determine hit endpoints and arrival times; unconfirmed shots retain
their launch trajectory. Toolkit-inferred victim IDs are diagnostic only.

`npm run build:bridge` produces the `/player/` bundle. New Bridge versions start
one background native webview per render job; older versions retain the in-page
save path. H.264 encoding uses an explicit frame clock. An optional byte budget
can retry at lower bitrate, checking actual MP4 length before accepting output.

Local Save video opens a settings dialog. All presets use H.264 MP4, 1080p and
10× battle speed: Compact (30 fps / 2 Mbps), Balanced (30 fps / 4 Mbps, default),
Sharper (30 fps / 6 Mbps), and Smooth (60 fps / 6 Mbps). Resolution (1080p/1440p),
frame rate (24/30/60), bitrate (1–12 Mbps) and speed (5×/10×/20×) are adjustable.
The duration/size estimate is approximate; encoder output is variable. Native
saves go to the user's Videos/TFD Bridge Renders directory. Rendering shows a
blocking, cancellable progress dialog instead of changing the toolbar layout.

Frame capture copies the live map canvas directly and reuses the HUD raster
while its DOM is unchanged. This avoids PNG encoding/decoding of every map frame
and repeated per-element style computation. Hardware encoding is preferred via
WebCodecs, with software fallback; the preference is a hint, not proof of a
particular hardware backend. H.264 level selection considers frame rate as well
as resolution (1080p60 uses L4.2).

Local job routes (only enabled with a native renderer dispatcher):

`/v1/health` includes `local-render-jobs-v1` only when that dispatcher is
configured. This capability does not imply Discord authorization or delivery.

- `POST /v1/render/jobs`: replay_name, optional arena_unique_id decimal string,
  optional max_bytes and video_options `{resolution,fps,bitrate,speed}`.
  Video options are local-save only; share jobs retain the Engine byte-budget
  policy. Same-origin player only. Complete, local replay required.
- `GET /v1/render/jobs/{id}`: state, attempt, frame, total, bytes and error code.
- `POST /v1/render/jobs/{id}/cancel`: request cancellation. An active worker must
  stop before its slot is released.
- Worker-only claim/progress/video/fail/stopped actions require a per-job
  capability. Public status omits that capability and filesystem paths.

The Replayer save button uses this path, with a fallback for older Bridge builds.
Native output lives in the Bridge video folder; repeat saves never overwrite an
existing file. Native worker loss or stalled progress is bounded by its owner.

Native builds with the sharing coordinator also expose `replay-video-share-v1`.
The Replayer's Share replay video action opens Engine authorization. The Engine
post-battle screen can submit its authorized share directly. Both paths resolve
the local replay, render to the supplied byte budget, request a fresh upload
ticket, upload natively, complete the share and poll delivery status without
keeping the browser tab open. See [video-sharing.md](../docs/video-sharing.md)
for payloads and testing boundaries. Engine owns storage, bot delivery and
immediate deletion of its temporary object after posting. No live Discord post
was performed during local validation.
