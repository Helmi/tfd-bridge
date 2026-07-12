# Replay Web Player experiment

An intentionally isolated, uncommitted UI experiment for a browser-native World of Warships replay viewer.

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
is experiment scaffolding; the eventual product route belongs in the Bridge's
read-only loopback API and background decode queue.

The decoder should emit this shape (or an evolution of it) in time chunks. It should not emit PixiJS commands. WoWS track coordinates are currently normalized `0..1`; `map.spaceSize` is retained as metadata, not used as viewer bounds. Speed is shown as unknown unless the exporter supplies a trustworthy `speedKnots` value.
