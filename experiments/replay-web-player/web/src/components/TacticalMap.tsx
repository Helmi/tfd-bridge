import { useEffect, useRef, useState } from 'react';
import { Application, Assets, Container, Graphics, Sprite, Text, Texture } from 'pixi.js';
import { evaluateScene } from '../engine/timeline';
import { shipClassIconUrl, type ShipClass } from '../shipClassIcons';
import type { EvaluatedShip, PlaneTrack, ReplayScene, SceneState, WorldPoint } from '../types';

interface Props {
  scene: ReplayScene;
  time: number;
  selectedShipId: string;
  onSelectShip: (id: string) => void;
}

interface Viewport {
  left: number;
  top: number;
  size: number;
}

interface Runtime {
  app: Application;
  backdrop: Graphics;
  mapSprite: Sprite;
  graphics: Graphics;
  markers: Container;
  iconTextures: Partial<Record<ShipClass, Texture>>;
  powerupTextures: Record<string, Texture>;
  powerupAssetsKey?: string;
  planeTextures: Record<string, Texture>;
  planeAssetsKey?: string;
  mapHref?: string;
  viewport: Viewport;
  latestState: SceneState;
  scene: ReplayScene;
  selectedShipId: string;
}

const islands: WorldPoint[][] = [
  [{ x: 80, y: 205 }, { x: 146, y: 164 }, { x: 210, y: 190 }, { x: 232, y: 258 }, { x: 177, y: 285 }, { x: 103, y: 262 }],
  [{ x: 382, y: 70 }, { x: 442, y: 52 }, { x: 489, y: 91 }, { x: 477, y: 151 }, { x: 414, y: 164 }, { x: 373, y: 125 }],
  [{ x: 672, y: 203 }, { x: 732, y: 163 }, { x: 815, y: 179 }, { x: 846, y: 246 }, { x: 789, y: 278 }, { x: 706, y: 263 }],
  [{ x: 302, y: 418 }, { x: 358, y: 378 }, { x: 411, y: 407 }, { x: 429, y: 471 }, { x: 377, y: 499 }, { x: 321, y: 477 }],
  [{ x: 573, y: 508 }, { x: 625, y: 467 }, { x: 685, y: 490 }, { x: 701, y: 552 }, { x: 642, y: 581 }, { x: 589, y: 557 }],
  [{ x: 77, y: 681 }, { x: 139, y: 636 }, { x: 213, y: 661 }, { x: 225, y: 731 }, { x: 174, y: 766 }, { x: 101, y: 744 }],
  [{ x: 385, y: 767 }, { x: 448, y: 725 }, { x: 514, y: 750 }, { x: 527, y: 819 }, { x: 469, y: 851 }, { x: 405, y: 827 }],
  [{ x: 742, y: 800 }, { x: 798, y: 758 }, { x: 876, y: 783 }, { x: 898, y: 850 }, { x: 843, y: 883 }, { x: 771, y: 864 }],
];

function makeViewport(width: number, height: number): Viewport {
  const padding = Math.max(16, Math.min(width, height) * 0.025);
  const size = Math.max(1, Math.min(width, height) - padding * 2);
  return { left: (width - size) / 2, top: (height - size) / 2, size };
}

function worldToScreen(scene: ReplayScene, viewport: Viewport, point: WorldPoint): WorldPoint {
  const { minX, minY, maxX, maxY } = scene.map.bounds;
  return {
    x: viewport.left + ((point.x - minX) / (maxX - minX)) * viewport.size,
    y: viewport.top + ((point.y - minY) / (maxY - minY)) * viewport.size,
  };
}

function screenToWorld(scene: ReplayScene, viewport: Viewport, point: WorldPoint): WorldPoint {
  const { minX, minY, maxX, maxY } = scene.map.bounds;
  return {
    x: minX + ((point.x - viewport.left) / viewport.size) * (maxX - minX),
    y: minY + ((point.y - viewport.top) / viewport.size) * (maxY - minY),
  };
}

function shipScale(ship: EvaluatedShip, viewport: Viewport): number {
  const base = viewport.size / 760;
  const size = ship.definition.shipClass === 'battleship' ? 1.18
    : ship.definition.shipClass === 'destroyer' ? 0.86
      : ship.definition.shipClass === 'carrier' ? 1.24
        : ship.definition.shipClass === 'submarine' ? 0.82
          : 1;
  // Clamp the screen factor so markers stay legible on small windows and don't
  // balloon on large ones — the base marker icon is 16*scale px, so this holds
  // it to roughly 13–26px across viewport sizes while keeping class proportions.
  // Overall footprint trimmed ~20% (the 0.8) so markers sit better on the map.
  return 0.8 * Math.min(2.0, Math.max(1.05, base * size));
}

function transformedHull(center: WorldPoint, yaw: number, scale: number): number[] {
  const angle = (yaw * Math.PI) / 180;
  const cos = Math.cos(angle);
  const sin = Math.sin(angle);
  const points = [[0, -11], [5, -4], [4.5, 8], [0, 12], [-4.5, 8], [-5, -4]];
  return points.flatMap(([x, y]) => [center.x + (x * cos - y * sin) * scale, center.y + (x * sin + y * cos) * scale]);
}

function mapText(text: string, fontSize: number, color: string, weight: '500' | '600' | '700' | '800' = '600'): Text {
  return new Text({
    text,
    style: {
      fontFamily: '"Space Grotesk", "Segoe UI", sans-serif',
      fontSize,
      fontWeight: weight,
      fill: color,
      align: 'center',
      stroke: { color: '#05100d', width: Math.max(2, fontSize * 0.22) },
    },
  });
}

function drawCaptureZones(runtime: Runtime, teamColors: Record<string, string>): void {
  const { graphics, markers, scene, latestState, viewport } = runtime;
  for (const zone of latestState.captureZones) {
    if (!zone.enabled) continue;
    const center = worldToScreen(scene, viewport, zone.center);
    const radius = (zone.radius / (scene.map.bounds.maxX - scene.map.bounds.minX)) * viewport.size;
    const ownerColor = zone.owner ? teamColors[zone.owner] : undefined;
    const invaderColor = zone.hasInvaders && zone.invader ? teamColors[zone.invader] : undefined;
    const capturing = Boolean(invaderColor);
    const blocked = capturing && zone.contested;
    const outline = blocked ? '#ffbd66' : invaderColor ?? ownerColor ?? '#7b9189';
    const fill = ownerColor ?? '#122420';

    // Base ring: thin, in the owning/neutral color.
    graphics.circle(center.x, center.y, radius)
      .fill({ color: fill, alpha: ownerColor ? 0.14 : 0.08 })
      .stroke({ color: outline, width: 1.6, alpha: 0.6 });

    // Capture progress rides the ring itself — a sweeping arc from the top in
    // the capturing team's color (amber while contested/blocked).
    if (capturing) {
      const ratio = Math.max(0, Math.min(1, zone.progress / 100));
      if (ratio > 0) {
        graphics.beginPath();
        graphics.arc(center.x, center.y, radius, -Math.PI / 2, -Math.PI / 2 + Math.PI * 2 * ratio)
          .stroke({ color: outline, width: 3.4, alpha: 0.95 });
        graphics.beginPath();
      }
    }

    const label = mapText(zone.definition.label, Math.max(11, viewport.size * 0.019), outline, '600');
    label.anchor.set(0.5);
    label.position.set(center.x, center.y);
    markers.addChild(label);
  }
}

function drawBuffZones(runtime: Runtime, teamColors: Record<string, string>): void {
  const { graphics, markers, scene, latestState, viewport } = runtime;
  for (const zone of latestState.buffZones) {
    if (!zone.active) continue;
    const center = worldToScreen(scene, viewport, zone.center);
    const radius = (zone.radius / (scene.map.bounds.maxX - scene.map.bounds.minX)) * viewport.size;
    const color = zone.teamId ? teamColors[zone.teamId] ?? '#eef4f1' : '#eef4f1';

    // Zones with no Drop marker are not Arms Race powerups — they are generic
    // interactive zones (e.g. catapult-fighter patrol areas). Render them as a
    // faint patrol ring only; the fighter squadron itself is drawn separately.
    if (!zone.markerName) {
      graphics.circle(center.x, center.y, radius).stroke({ color, width: 1, alpha: 0.22 });
      continue;
    }

    const marker = new Container();
    marker.position.set(center.x, center.y);
    const ring = new Graphics();
    ring.circle(0, 0, radius)
      .stroke({ color, width: 1.2, alpha: zone.collectible ? 0.82 : 0.26 });
    if (!zone.collectible && zone.activationProgress > 0) {
      const steps = Math.max(2, Math.ceil(48 * zone.activationProgress));
      for (let step = 0; step <= steps; step += 1) {
        const angle = -Math.PI / 2 + Math.PI * 2 * zone.activationProgress * (step / steps);
        const x = Math.cos(angle) * radius;
        const y = Math.sin(angle) * radius;
        if (step === 0) ring.moveTo(x, y);
        else ring.lineTo(x, y);
      }
      ring.stroke({ color, width: 2.2, alpha: 0.9 });
    }
    marker.addChild(ring);

    const activeMarkerName = zone.markerName.replace(/_inactive$/i, '_active');
    const textureName = zone.collectible && runtime.powerupTextures[activeMarkerName]
      ? activeMarkerName
      : zone.markerName;
    const texture = runtime.powerupTextures[textureName];
    const iconSize = Math.max(18, Math.min(radius * 0.9, viewport.size * 0.04));
    if (texture) {
      const icon = new Sprite(texture);
      icon.anchor.set(0.5);
      icon.width = iconSize;
      icon.height = iconSize;
      icon.alpha = zone.collectible ? 1 : 0.78;
      marker.addChild(icon);
    } else {
      const fallback = mapText('✦', Math.max(10, iconSize * 0.5), color, '700');
      fallback.anchor.set(0.5);
      fallback.alpha = zone.collectible ? 1 : 0.72;
      marker.addChild(fallback);
    }
    markers.addChild(marker);
  }
}

function drawSmoke(runtime: Runtime): void {
  const { graphics, scene, latestState, viewport } = runtime;
  for (const smoke of latestState.smoke) {
    if (!smoke.active || !smoke.puffs.length) continue;
    const radius = (smoke.radius / (scene.map.bounds.maxX - scene.map.bounds.minX)) * viewport.size;
    for (const puff of smoke.puffs) {
      const center = worldToScreen(scene, viewport, puff);
      graphics.circle(center.x, center.y, radius).fill({ color: '#aec1ba', alpha: 0.16 });
      graphics.circle(center.x, center.y, radius * 0.72).fill({ color: '#c1d2c9', alpha: 0.14 });
    }
    for (const puff of smoke.puffs) {
      const center = worldToScreen(scene, viewport, puff);
      graphics.circle(center.x, center.y, radius).stroke({ color: '#d5e2dc', width: 0.8, alpha: 0.2 });
    }
  }
}

function dashedCircle(graphics: Graphics, x: number, y: number, radius: number, color: string, alpha: number): void {
  const segments = Math.max(18, Math.round(radius / 4));
  for (let segment = 0; segment < segments; segment += 1) {
    const from = (segment / segments) * Math.PI * 2;
    const to = from + (Math.PI * 2) / segments * 0.55;
    graphics.arc(x, y, radius, from, to).stroke({ color, width: 1.4, alpha });
    graphics.beginPath();
  }
}

function drawWards(runtime: Runtime, markers: Container, teamColors: Record<string, string>): void {
  const { graphics, scene, latestState, viewport } = runtime;
  const wardTexture = runtime.planeTextures.ward;
  for (const ward of latestState.wards) {
    if (!ward.active) continue;
    const center = worldToScreen(scene, viewport, ward.definition.center);
    const radius = (ward.definition.radius / (scene.map.bounds.maxX - scene.map.bounds.minX)) * viewport.size;
    const color = ward.definition.teamId ? teamColors[ward.definition.teamId] ?? '#9fe6d2' : '#9fe6d2';
    graphics.circle(center.x, center.y, radius).fill({ color, alpha: 0.05 });
    dashedCircle(graphics, center.x, center.y, radius, color, 0.55);
    if (wardTexture) {
      const size = Math.max(11, viewport.size * 0.017);
      const icon = new Sprite(wardTexture);
      icon.anchor.set(0.5);
      icon.width = size;
      icon.height = size;
      icon.position.set(center.x, center.y);
      icon.alpha = 0.9;
      markers.addChild(icon);
    }
  }
}

/** Aircraft silhouette pointing north, scaled/rotated like the torpedo hull. */
function transformedPlane(center: WorldPoint, headingDeg: number, scale: number): number[] {
  const angle = (headingDeg * Math.PI) / 180;
  const cos = Math.cos(angle);
  const sin = Math.sin(angle);
  const points = [
    [0, -7], [1.3, -2.4], [7, 0.2], [7, 2], [1.4, 1.4], [1, 4.4], [3, 5.9], [3, 7.1], [0, 6.2],
    [-3, 7.1], [-3, 5.9], [-1, 4.4], [-1.4, 1.4], [-7, 2], [-7, 0.2], [-1.3, -2.4],
  ];
  return points.flatMap(([x, y]) => [center.x + (x * cos - y * sin) * scale, center.y + (x * sin + y * cos) * scale]);
}

const PLANE_RELATION_SUFFIX: Record<string, string> = { self: 'own', ally: 'ally', enemy: 'enemy' };

function planeIconKey(def: PlaneTrack): string | undefined {
  const { iconDir, iconBase, relation } = def;
  if (!iconDir || !iconBase) return undefined;
  const suffix = PLANE_RELATION_SUFFIX[relation ?? 'ally'] ?? 'ally';
  return `${iconDir}/${iconBase}_${suffix}`;
}

function drawPlanes(runtime: Runtime, markers: Container, teamColors: Record<string, string>): void {
  const { graphics, scene, latestState, viewport } = runtime;
  for (const plane of latestState.planes) {
    if (!plane.active) continue;
    const center = worldToScreen(scene, viewport, plane.position);
    const key = planeIconKey(plane.definition);
    const texture = key ? runtime.planeTextures[key] : undefined;
    if (texture) {
      // Game squadron badge markers stay upright (they are type glyphs, not
      // directional silhouettes) and small.
      const size = Math.max(9, viewport.size * 0.0135);
      const icon = new Sprite(texture);
      icon.anchor.set(0.5);
      icon.width = size;
      icon.height = size;
      icon.position.set(center.x, center.y);
      icon.alpha = plane.definition.category === 'consumable' ? 0.9 : 1;
      markers.addChild(icon);
    } else {
      const color = teamColors[plane.definition.teamId] ?? '#d9fff2';
      const scale = Math.max(0.55, viewport.size / 1300);
      graphics.poly(transformedPlane(center, plane.heading, scale))
        .fill({ color, alpha: 0.9 }).stroke({ color: '#0a1a21', width: 0.8, alpha: 0.85 });
    }
  }
}

// Shell tracer color by ammunition type: HE yellow, AP white, SAP red.
function shellColor(ammoType: string | undefined): string {
  switch (ammoType) {
    case 'HE': return '#ffd633';
    case 'AP': return '#ffffff';
    case 'SAP': return '#ff5347';
    default: return '#eef4f1';
  }
}

/** Slim torpedo silhouette pointing north — longer and narrower than a hull. */
function transformedTorpedo(center: WorldPoint, headingDeg: number, scale: number): number[] {
  const angle = (headingDeg * Math.PI) / 180;
  const cos = Math.cos(angle);
  const sin = Math.sin(angle);
  const points = [[0, -9], [1.4, -6], [1.6, 6], [0.9, 9], [-0.9, 9], [-1.6, 6], [-1.4, -6]];
  return points.flatMap(([x, y]) => [center.x + (x * cos - y * sin) * scale, center.y + (x * sin + y * cos) * scale]);
}

function drawMap(runtime: Runtime): void {
  const { app, backdrop, mapSprite, graphics, markers, scene, latestState, selectedShipId } = runtime;
  const viewport = makeViewport(app.screen.width, app.screen.height);
  runtime.viewport = viewport;
  backdrop.clear();
  graphics.clear();
  for (const child of markers.removeChildren()) child.destroy();

  backdrop.roundRect(viewport.left - 2, viewport.top - 2, viewport.size + 4, viewport.size + 4, 8)
    .fill({ color: '#081210' })
    .stroke({ color: '#2a3d36', width: 1.2, alpha: 0.85 });

  mapSprite.position.set(viewport.left, viewport.top);
  mapSprite.width = viewport.size;
  mapSprite.height = viewport.size;
  mapSprite.alpha = 0.8;

  const gridSize = viewport.size / 10;
  for (let index = 1; index < 10; index += 1) {
    const offset = index * gridSize;
    graphics.moveTo(viewport.left + offset, viewport.top).lineTo(viewport.left + offset, viewport.top + viewport.size)
      .stroke({ color: '#5f8074', width: 0.7, alpha: 0.12 });
    graphics.moveTo(viewport.left, viewport.top + offset).lineTo(viewport.left + viewport.size, viewport.top + offset)
      .stroke({ color: '#5f8074', width: 0.7, alpha: 0.12 });
  }

  if (scene.replay.source === 'synthetic') {
    for (const island of islands) {
      const points = island.flatMap((point) => {
        const screen = worldToScreen(scene, viewport, point);
        return [screen.x, screen.y];
      });
      backdrop.poly(points).fill({ color: '#27433d', alpha: 0.92 }).stroke({ color: '#58806a', width: 1, alpha: 0.65 });
    }
  }

  const teamColors = Object.fromEntries(scene.teams.map((team) => [team.id, team.color]));
  // Torpedoes are colored by side so incoming enemy fish read as a threat:
  // enemy = enemy red, friendly (own/ally) = green. Derived from the roster.
  const enemyTeamIds = new Set(
    scene.ships.filter((ship) => ship.relation === 'enemy').map((ship) => ship.teamId),
  );
  drawCaptureZones(runtime, teamColors);
  drawBuffZones(runtime, teamColors);
  drawSmoke(runtime);
  drawWards(runtime, markers, teamColors);

  for (const ordnance of latestState.ordnance) {
    const position = worldToScreen(scene, viewport, ordnance.position);
    const headingRad = (ordnance.heading * Math.PI) / 180;
    const forward = { x: Math.sin(headingRad), y: -Math.cos(headingRad) };
    if (ordnance.event.kind === 'shell') {
      // Short stroke along the flight direction, colored by shell type — no
      // long trail, no head dot.
      const strokeLength = viewport.size * 0.009;
      const tail = { x: position.x - forward.x * strokeLength, y: position.y - forward.y * strokeLength };
      const color = shellColor(ordnance.event.ammoType);
      graphics.moveTo(tail.x, tail.y).lineTo(position.x, position.y).stroke({ color, width: 1, alpha: 0.85 });
    } else {
      // Torpedoes read as a capsule trailing a faint wake, colored by side
      // (enemy red / friendly green); the armed state is carried by alpha below.
      const torpedoColor = enemyTeamIds.has(ordnance.event.teamId) ? '#f2665c' : '#4fe0a0';
      const wakeLength = viewport.size * 0.03;
      const wakeTail = { x: position.x - forward.x * wakeLength, y: position.y - forward.y * wakeLength };
      graphics.moveTo(wakeTail.x, wakeTail.y).lineTo(position.x, position.y)
        .stroke({ color: torpedoColor, width: 1.1, alpha: ordnance.armed ? 0.34 : 0.2 });
      const torpedoHull = transformedTorpedo(position, ordnance.heading, Math.max(0.34, viewport.size / 1650));
      graphics.poly(torpedoHull).fill({ color: torpedoColor, alpha: ordnance.armed ? 0.95 : 0.6 })
        .stroke({ color: '#0a1a21', width: 0.8, alpha: 0.85 });
    }
  }

  for (const ship of latestState.ships) {
    if (ship.knowledge === 'hidden') continue;
    const point = worldToScreen(scene, viewport, ship.displayPose);
    const color = teamColors[ship.definition.teamId];
    const scale = shipScale(ship, viewport);
    const selected = ship.definition.id === selectedShipId;
    const alpha = ship.knowledge === 'last-known' ? 0.42 : ship.destroyed ? 0.3 : 1;

    if (selected) {
      graphics.circle(point.x, point.y, 18 * scale).fill({ color, alpha: 0.08 }).stroke({ color: '#ffffff', width: 1.7, alpha: 0.9 });
    }

    if (ship.detectedByEnemy && ship.definition.relation !== 'enemy' && !ship.destroyed) {
      graphics.circle(point.x, point.y, 16 * scale).fill({ color: '#ffbd66', alpha: 0.14 });
    }

    const texture = runtime.iconTextures[ship.definition.shipClass];
    if (texture) {
      const icon = new Sprite(texture);
      icon.anchor.set(0.5);
      icon.height = 16 * scale;
      icon.width = icon.height * (texture.width / texture.height);
      icon.position.set(point.x, point.y);
      icon.rotation = (ship.displayPose.yaw * Math.PI) / 180;
      icon.tint = color;
      icon.alpha = alpha;
      markers.addChild(icon);
    } else {
      const hull = transformedHull(point, ship.displayPose.yaw, scale);
      graphics.poly(hull).fill({ color, alpha }).stroke({ color: '#eef4f1', width: selected ? 1.35 : 0.75, alpha: 0.8 * alpha });
    }

    if (ship.knowledge === 'last-known') {
      graphics.circle(point.x, point.y, 14 * scale).stroke({ color, width: 1, alpha: 0.42 });
    }

    // Health as a ring around the marker, only once the ship has taken damage.
    // A full-health ship stays clean; the ring is slightly thicker than the
    // thin last-known border so remaining HP reads at a glance.
    const hpRatio = Math.max(0, ship.health / ship.definition.maxHealth);
    if (!ship.destroyed && hpRatio < 0.995) {
      const hpRadius = 13 * scale;
      const hpColor = hpRatio > 0.55 ? '#4fe0a0' : hpRatio > 0.25 ? '#ffd369' : '#ff665f';
      graphics.circle(point.x, point.y, hpRadius).stroke({ color: '#0a1411', width: 2.4, alpha: 0.55 });
      graphics.beginPath();
      graphics.arc(point.x, point.y, hpRadius, -Math.PI / 2, -Math.PI / 2 + Math.PI * 2 * hpRatio)
        .stroke({ color: hpColor, width: 2.4, alpha: 0.95 * alpha });
      graphics.beginPath();
    }

    if (ship.destroyed) {
      const cross = 7 * scale;
      graphics.moveTo(point.x - cross, point.y - cross).lineTo(point.x + cross, point.y + cross)
        .moveTo(point.x + cross, point.y - cross).lineTo(point.x - cross, point.y + cross)
        .stroke({ color: '#d5e2dc', width: 1.8, alpha: 0.75 });
    }

    const name = mapText(
      ship.definition.shipName,
      Math.max(10, Math.min(13, viewport.size / 55)),
      selected ? '#ffffff' : color,
      selected ? '700' : '600',
    );
    name.anchor.set(0.5, 0);
    name.position.set(point.x, point.y + 15 * scale);
    name.alpha = ship.knowledge === 'last-known' ? 0.56 : ship.destroyed ? 0.42 : 0.95;
    markers.addChild(name);
  }

  drawPlanes(runtime, markers, teamColors);
}

async function loadMapImage(runtime: Runtime): Promise<void> {
  const href = runtime.scene.map.image?.href;
  if (!href) {
    runtime.mapHref = undefined;
    runtime.mapSprite.visible = false;
    drawMap(runtime);
    return;
  }
  if (runtime.mapHref === href && runtime.mapSprite.visible) return;
  runtime.mapHref = href;
  runtime.mapSprite.visible = false;
  try {
    const texture = await Assets.load<Texture>(href);
    if (runtime.mapHref !== href) return;
    runtime.mapSprite.texture = texture;
    runtime.mapSprite.visible = true;
    drawMap(runtime);
  } catch (reason) {
    console.warn('Replay map image could not be loaded', reason);
  }
}

async function loadShipIcons(runtime: Runtime): Promise<void> {
  const classes: ShipClass[] = ['destroyer', 'cruiser', 'battleship', 'carrier', 'submarine'];
  const loaded = await Promise.all(classes.map(async (shipClass) => {
    const href = new URL(shipClassIconUrl(shipClass), window.location.href).href;
    return [shipClass, await Assets.load<Texture>(href)] as const;
  }));
  runtime.iconTextures = Object.fromEntries(loaded);
  drawMap(runtime);
}

async function loadPowerupIcons(runtime: Runtime): Promise<void> {
  const entries = Object.entries(runtime.scene.assets?.powerupIcons ?? {}).sort(([left], [right]) => left.localeCompare(right));
  const key = JSON.stringify(entries.map(([markerName, asset]) => [markerName, asset.href]));
  if (runtime.powerupAssetsKey === key) return;
  runtime.powerupAssetsKey = key;
  runtime.powerupTextures = {};
  if (!entries.length) {
    drawMap(runtime);
    return;
  }

  const loaded = await Promise.all(entries.map(async ([markerName, asset]) => {
    try {
      return [markerName, await Assets.load<Texture>(asset.href)] as const;
    } catch (reason) {
      console.warn(`Powerup icon ${markerName} could not be loaded`, reason);
      return undefined;
    }
  }));
  if (runtime.powerupAssetsKey !== key) return;
  runtime.powerupTextures = Object.fromEntries(loaded.filter((entry): entry is readonly [string, Texture] => Boolean(entry)));
  drawMap(runtime);
}

function planeMarkerUrl(key: string): string {
  return new URL(`${import.meta.env.BASE_URL}assets/plane-markers/${key}.png`, window.location.href).href;
}

// Squadron and ward markers are the game's tactical-map icon set, bundled as
// static assets (constant per game version) and loaded by descriptor key.
async function loadPlaneIcons(runtime: Runtime): Promise<void> {
  const keys = new Set<string>();
  for (const plane of runtime.scene.planes ?? []) {
    const key = planeIconKey(plane);
    if (key) keys.add(key);
  }
  if (runtime.scene.wards?.length) keys.add('ward');
  const sorted = [...keys].sort();
  const cacheKey = sorted.join('|');
  if (runtime.planeAssetsKey === cacheKey) return;
  runtime.planeAssetsKey = cacheKey;
  runtime.planeTextures = {};
  if (!sorted.length) {
    drawMap(runtime);
    return;
  }

  const loaded = await Promise.all(sorted.map(async (key) => {
    try {
      const texture = await Assets.load<Texture>(planeMarkerUrl(key));
      texture.source.scaleMode = 'linear';
      return [key, texture] as const;
    } catch (reason) {
      console.warn(`Plane marker ${key} could not be loaded`, reason);
      return undefined;
    }
  }));
  if (runtime.planeAssetsKey !== cacheKey) return;
  runtime.planeTextures = Object.fromEntries(loaded.filter((entry): entry is readonly [string, Texture] => Boolean(entry)));
  drawMap(runtime);
}

export function TacticalMap({ scene, time, selectedShipId, onSelectShip }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const runtimeRef = useRef<Runtime | null>(null);
  const onSelectRef = useRef(onSelectShip);
  const [error, setError] = useState<string>();
  onSelectRef.current = onSelectShip;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let disposed = false;
    const app = new Application();

    const initialize = async () => {
      try {
        await app.init({
          preference: 'webgl',
          resizeTo: host,
          antialias: true,
          autoDensity: true,
          resolution: Math.min(window.devicePixelRatio || 1, 2),
          backgroundAlpha: 0,
        });
        if (disposed) {
          app.destroy(true);
          return;
        }

        app.canvas.className = 'tactical-canvas';
        host.appendChild(app.canvas);
        const backdrop = new Graphics();
        const mapSprite = new Sprite(Texture.EMPTY);
        mapSprite.visible = false;
        const graphics = new Graphics();
        const markers = new Container();
        const stage = new Container();
        stage.addChild(backdrop, mapSprite, graphics, markers);
        app.stage.addChild(stage);
        runtimeRef.current = {
          app,
          backdrop,
          mapSprite,
          graphics,
          markers,
          iconTextures: {},
          powerupTextures: {},
          planeTextures: {},
          scene,
          selectedShipId,
          latestState: evaluateScene(scene, time),
          viewport: makeViewport(app.screen.width, app.screen.height),
        };
        drawMap(runtimeRef.current);
        void loadMapImage(runtimeRef.current);
        void loadShipIcons(runtimeRef.current).catch((reason) => console.warn('Ship-class icons could not be loaded', reason));
        void loadPowerupIcons(runtimeRef.current);
        void loadPlaneIcons(runtimeRef.current);

        const pickShip = (event: PointerEvent) => {
          const runtime = runtimeRef.current;
          if (!runtime) return;
          const rect = app.canvas.getBoundingClientRect();
          const screen = { x: event.clientX - rect.left, y: event.clientY - rect.top };
          const world = screenToWorld(runtime.scene, runtime.viewport, screen);
          const worldRadius = (runtime.scene.map.bounds.maxX - runtime.scene.map.bounds.minX) * 0.035;
          const candidate = runtime.latestState.ships
            .filter((ship) => ship.knowledge !== 'hidden')
            .map((ship) => ({ ship, distance: Math.hypot(ship.displayPose.x - world.x, ship.displayPose.y - world.y) }))
            .filter(({ distance }) => distance <= worldRadius)
            .sort((left, right) => left.distance - right.distance)[0];
          if (candidate) onSelectRef.current(candidate.ship.definition.id);
        };
        app.canvas.addEventListener('pointerdown', pickShip);

        const resizeObserver = new ResizeObserver(() => {
          requestAnimationFrame(() => {
            if (runtimeRef.current) drawMap(runtimeRef.current);
          });
        });
        resizeObserver.observe(host);
        (runtimeRef.current as Runtime & { cleanup?: () => void }).cleanup = () => {
          resizeObserver.disconnect();
          app.canvas.removeEventListener('pointerdown', pickShip);
        };
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    };

    void initialize();
    return () => {
      disposed = true;
      const runtime = runtimeRef.current as (Runtime & { cleanup?: () => void }) | null;
      runtime?.cleanup?.();
      runtimeRef.current = null;
      if (app.renderer) app.destroy(true, { children: true });
    };
  // Pixi owns its lifecycle; scene changes are handled by the drawing effect below.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const runtime = runtimeRef.current;
    if (!runtime) return;
    runtime.scene = scene;
    runtime.selectedShipId = selectedShipId;
    runtime.latestState = evaluateScene(scene, time);
    drawMap(runtime);
    void loadMapImage(runtime);
    void loadPowerupIcons(runtime);
    void loadPlaneIcons(runtime);
  }, [scene, time, selectedShipId]);

  return (
    <div className="map-host" ref={hostRef}>
      {error && (
        <div className="renderer-error">
          <strong>The tactical map could not start</strong>
          <span>{error}</span>
        </div>
      )}
    </div>
  );
}
