import { Assets, Container, Graphics, Rectangle, RenderTexture, Sprite, Texture, type Text, type TextStyleOptions } from 'pixi.js';
import { isOwnDivision, shipIconColor } from '../divisions';
import { createShipMarker } from '../components/shipMarker';
import { updateShipLabel } from '../components/shipLabel';
import { consumableIconKey, consumableIconUrl } from '../consumableIcons';
import { activeConsumableVisuals } from '../engine/consumableVisuals';
import { drawConsumableRanges, drawConsumableIcons } from '../components/consumableEffects';
import { acquireEnhancedMapTexture, type EnhancedMapTextureLease } from '../engine/enhancedMap';
import { clipRpfSector } from '../engine/rpfGeometry';
import { shipClassIconUrl, type ShipClass } from '../shipClassIcons';
import type { EntityId, EvaluatedShip, PlaneTrack, ReplayScene, SceneState, WorldPoint } from '../types';
import { BOX } from './layout';
import { setTextIfChanged } from './text';
import type { BroadcastLayer, StageContext } from './types';

export type MapPresentation = 'interactive' | 'broadcast';

export interface MapViewport {
  left: number;
  top: number;
  size: number;
}

/** What the map layer needs from its host: the stage context, or the DOM path's own Application. */
export type MapLayerDeps = Pick<StageContext, 'renderer' | 'resolution' | 'mapSupersampling' | 'toFrame' | 'createText'> & {
  onSelect(id: EntityId): void;
};

export interface MapDrawInput {
  scene: ReplayScene;
  state: SceneState;
  selectedId: EntityId;
  presentation: MapPresentation;
  /** Size of the area the map viewport is centred in. */
  width: number;
  height: number;
}

export interface MapLayer extends BroadcastLayer {
  ready(): Promise<void>;
  /** Draw a state with explicit settings. `update(frame)` calls this with the stage box. */
  draw(input: MapDrawInput): void;
}

/** Deps for the stage. The getters keep `renderer` and `resolution` live. Clicks go through `frame.selectShip`. */
export function mapLayerDeps(ctx: StageContext): MapLayerDeps {
  return {
    get renderer() { return ctx.renderer; },
    get resolution() { return ctx.resolution; },
    mapSupersampling: ctx.mapSupersampling,
    toFrame: ctx.toFrame,
    createText: ctx.createText,
    onSelect: () => {},
  };
}

interface Runtime {
  deps: MapLayerDeps;
  content: Container;
  backdrop: Graphics;
  mapSprite: Sprite;
  graphics: Graphics;
  markers: Container;
  consumableGraphics: Graphics;
  consumableMask: Graphics;
  consumableTextures: Record<string, Texture>;
  iconTextures: Partial<Record<ShipClass, Texture>>;
  powerupTextures: Record<string, Texture>;
  powerupAssetsKey?: string;
  planeTextures: Record<string, Texture>;
  planeAssetsKey?: string;
  mapHref?: string;
  mapLease?: EnhancedMapTextureLease;
  mapRequest?: symbol;
  mapLoading?: Promise<void>;
  disposed?: boolean;
  viewport: MapViewport;
  width: number;
  height: number;
  latestState?: SceneState;
  scene: ReplayScene;
  selectedShipId: string;
  presentation: MapPresentation;
  zoneLabels: Pool<Text>;
  glyphs: Pool<Text>;
  shipLabels: Pool<ShipLabelEntry>;
  /** Present with `mapSupersampling`: the content is drawn into `texture` and shown through `sprite`. */
  supersample?: { sprite: Sprite; texture?: RenderTexture };
}

type DrawRuntime = Runtime & { latestState: SceneState };
const hasState = (runtime: Runtime): runtime is DrawRuntime => runtime.latestState !== undefined;

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

function makeViewport(width: number, height: number): MapViewport {
  const padding = Math.max(16, Math.min(width, height) * 0.025);
  const size = Math.max(1, Math.min(width, height) - padding * 2);
  return { left: (width - size) / 2, top: (height - size) / 2, size };
}

function worldToScreen(scene: ReplayScene, viewport: MapViewport, point: WorldPoint): WorldPoint {
  const { minX, minY, maxX, maxY } = scene.map.bounds;
  return {
    x: viewport.left + ((point.x - minX) / (maxX - minX)) * viewport.size,
    y: viewport.top + ((point.y - minY) / (maxY - minY)) * viewport.size,
  };
}

function screenToWorld(scene: ReplayScene, viewport: MapViewport, point: WorldPoint): WorldPoint {
  const { minX, minY, maxX, maxY } = scene.map.bounds;
  return {
    x: minX + ((point.x - viewport.left) / viewport.size) * (maxX - minX),
    y: minY + ((point.y - viewport.top) / viewport.size) * (maxY - minY),
  };
}

function shipScale(ship: EvaluatedShip, viewport: MapViewport): number {
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

type TextWeight = '500' | '600' | '700' | '800';

interface TextSpec {
  fontFamily: string;
  fontSize: number;
  fill: string;
  weight: TextWeight;
  strokeWidth: number;
  letterSpacing: number;
}

const MAP_FONT = '"Space Grotesk", "Segoe UI", sans-serif';
const BROADCAST_FONT = '"JetBrains Mono", monospace';

function mapTextSpec(fontSize: number, fill: string, weight: TextWeight = '600'): TextSpec {
  return { fontFamily: MAP_FONT, fontSize, fill, weight, strokeWidth: Math.max(2, fontSize * 0.22), letterSpacing: 0 };
}

function textOptions(spec: TextSpec): TextStyleOptions {
  return {
    fontFamily: spec.fontFamily,
    fontSize: spec.fontSize,
    fontWeight: spec.weight,
    fill: spec.fill,
    align: 'center',
    letterSpacing: spec.letterSpacing,
    stroke: { color: '#05100d', width: spec.strokeWidth },
  };
}

const specKey = (spec: TextSpec) => `${spec.fontFamily}|${spec.fontSize}|${spec.fill}|${spec.weight}|${spec.strokeWidth}|${spec.letterSpacing}`;
const appliedSpec = new WeakMap<object, string>();

function createMapText(deps: MapLayerDeps, spec: TextSpec): Text {
  const text = deps.createText(textOptions(spec));
  appliedSpec.set(text, specKey(spec));
  return text;
}

/** Style a pooled text. Pixi re-renders the text texture on every style write, so write only on change. */
function styleMapText(text: Text, spec: TextSpec): void {
  const key = specKey(spec);
  if (appliedSpec.get(text) === key) return;
  appliedSpec.set(text, key);
  text.style.fontFamily = spec.fontFamily;
  text.style.fontSize = spec.fontSize;
  text.style.fontWeight = spec.weight;
  text.style.fill = spec.fill;
  text.style.letterSpacing = spec.letterSpacing;
  text.style.stroke = { color: '#05100d', width: spec.strokeWidth };
}

/** Keyed display objects reused across draws. Entries not requested between `begin` and `sweep` are destroyed. */
class Pool<T> {
  private readonly items = new Map<string, T>();
  private readonly used = new Set<string>();
  private readonly roots = new Set<Container>();
  constructor(
    private readonly create: () => T,
    private readonly rootOf: (item: T) => Container,
    private readonly dispose: (item: T) => void,
  ) {}

  begin(): void { this.used.clear(); }

  get(key: string): T {
    this.used.add(key);
    let item = this.items.get(key);
    if (!item) {
      item = this.create();
      this.items.set(key, item);
      this.roots.add(this.rootOf(item));
    }
    return item;
  }

  sweep(): void {
    for (const [key, item] of this.items) {
      if (this.used.has(key)) continue;
      this.items.delete(key);
      this.roots.delete(this.rootOf(item));
      this.dispose(item);
    }
  }

  owns(display: Container): boolean { return this.roots.has(display); }
  values(): IterableIterator<T> { return this.items.values(); }

  clear(): void {
    for (const item of this.items.values()) this.dispose(item);
    this.items.clear();
    this.roots.clear();
    this.used.clear();
  }
}

interface ShipLabelEntry { root: Container; name: Text }

function drawRpfSectors(runtime: DrawRuntime): void {
  const { graphics, scene, latestState, viewport } = runtime;
  for (const sector of latestState.rpfSectors) {
    const clipped = clipRpfSector(sector.position, scene.map.bounds, sector);
    if (clipped.polygon.length < 3) continue;
    const points = clipped.polygon.flatMap(point => {
      const screen = worldToScreen(scene, viewport, point);
      return [screen.x, screen.y];
    });
    graphics.poly(points).fill({ color: '#d4e9e5', alpha: 0.085 });
    for (const [from, to] of clipped.rays) {
      const start = worldToScreen(scene, viewport, from);
      const end = worldToScreen(scene, viewport, to);
      graphics.moveTo(start.x, start.y).lineTo(end.x, end.y)
        .stroke({ color: '#d4e9e5', width: 1.1, alpha: 0.3 });
    }
  }
}

function drawCaptureZones(runtime: DrawRuntime, teamColors: Record<string, string>): void {
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

    const spec = mapTextSpec(Math.max(11, viewport.size * 0.019), outline, '600');
    const label = runtime.zoneLabels.get(zone.definition.id);
    setTextIfChanged(label, zone.definition.label);
    styleMapText(label, spec);
    label.position.set(center.x, center.y);
    markers.addChild(label);
  }
}

function drawBuffZones(runtime: DrawRuntime, teamColors: Record<string, string>): void {
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
      const fallback = runtime.glyphs.get(zone.definition.id);
      styleMapText(fallback, mapTextSpec(Math.max(10, iconSize * 0.5), color, '700'));
      fallback.alpha = zone.collectible ? 1 : 0.72;
      marker.addChild(fallback);
    }
    markers.addChild(marker);
  }
}

function drawSmoke(runtime: DrawRuntime): void {
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

function drawWards(runtime: DrawRuntime, markers: Container, teamColors: Record<string, string>): void {
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

function drawPlanes(runtime: DrawRuntime, markers: Container, teamColors: Record<string, string>): void {
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
  if (runtime.disposed || !hasState(runtime)) return;
  const { backdrop, mapSprite, graphics, markers, scene, latestState, selectedShipId } = runtime;
  const viewport = makeViewport(runtime.width, runtime.height);
  runtime.viewport = viewport;
  backdrop.clear();
  graphics.clear();
  // Pooled texts survive the draw; everything else is rebuilt.
  for (const glyph of runtime.glyphs.values()) glyph.parent?.removeChild(glyph);
  for (const child of markers.removeChildren()) {
    if (!runtime.zoneLabels.owns(child) && !runtime.shipLabels.owns(child)) child.destroy({children:true});
  }
  runtime.zoneLabels.begin();
  runtime.glyphs.begin();
  runtime.shipLabels.begin();

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
  const consumableEffects = activeConsumableVisuals(latestState);
  runtime.consumableMask.clear().rect(viewport.left,viewport.top,viewport.size,viewport.size).fill(0xffffff);
  drawConsumableRanges(runtime.consumableGraphics, consumableEffects, viewport.size, p=>worldToScreen(scene,viewport,p));
  drawRpfSectors(runtime);
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
      // Secondary-battery shells: ~60% length, about half the alpha.
      const secondary = ordnance.event.secondary === true;
      const strokeLength = viewport.size * (secondary ? 0.0054 : 0.009);
      const tail = { x: position.x - forward.x * strokeLength, y: position.y - forward.y * strokeLength };
      const color = shellColor(ordnance.event.ammoType);
      graphics.moveTo(tail.x, tail.y).lineTo(position.x, position.y).stroke({ color, width: 1, alpha: secondary ? 0.42 : 0.85 });
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
    const iconColor = shipIconColor(ship.definition, scene.ships, selectedShipId, color);
    const scale = shipScale(ship, viewport) * (runtime.presentation === 'broadcast' ? 1.2 : 1);
    const selected = ship.definition.id === selectedShipId;
    const alpha = ship.knowledge === 'last-known' ? 0.42 : ship.destroyed ? 0.3 : 1;

    if (ship.detectedByEnemy && ship.definition.relation !== 'enemy' && !ship.destroyed) {
      graphics.circle(point.x, point.y, 16 * scale).fill({ color: '#ffbd66', alpha: 0.14 });
    }

    markers.addChild(createShipMarker({
      texture: runtime.iconTextures[ship.definition.shipClass], shipClass: ship.definition.shipClass,
      position: point, yaw: ship.displayPose.yaw, scale, color: iconColor, alpha, selected,
    }));

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

    const broadcast = runtime.presentation === 'broadcast';
    const nameSize = broadcast ? 13 : Math.max(10, Math.min(13, viewport.size / 55));
    const nameSpec: TextSpec = broadcast
      ? { fontFamily: BROADCAST_FONT, fontSize: nameSize, fill: iconColor, weight: selected ? '600' : '500', strokeWidth: 2.5, letterSpacing: -0.35 }
      : mapTextSpec(nameSize, iconColor, selected ? '700' : '600');
    const entry = runtime.shipLabels.get(ship.definition.id);
    setTextIfChanged(entry.name, broadcast ? ship.definition.shipName.toUpperCase() : ship.definition.shipName);
    styleMapText(entry.name, nameSpec);
    updateShipLabel(entry.root, entry.name, {
      point, viewport, scale, color: iconColor,
      ownDivision: isOwnDivision(ship.definition, scene.ships),
      alpha: ship.knowledge === 'last-known' ? 0.56 : ship.destroyed ? 0.42 : 0.95,
    });
    markers.addChild(entry.root);
  }

  drawConsumableIcons(markers,consumableEffects,runtime.consumableTextures,viewport,
    p=>worldToScreen(scene,viewport,p),ship=>shipScale(ship,viewport)*(runtime.presentation==='broadcast'?1.2:1));
  drawPlanes(runtime, markers, teamColors);
  runtime.zoneLabels.sweep();
  runtime.glyphs.sweep();
  runtime.shipLabels.sweep();
  renderSupersampled(runtime);
}

function releaseMapTexture(runtime: Runtime): void {
  runtime.mapSprite.texture = Texture.EMPTY;
  runtime.mapLease?.release();
  runtime.mapLease = undefined;
}

function loadMapImage(runtime: Runtime): Promise<void> {
  if (runtime.disposed) return Promise.resolve();
  const href = runtime.scene.map.image?.href;
  const mapName = runtime.scene.map.name;
  const key = `${mapName}\0${href ?? ''}`;
  if (runtime.mapHref === key) {
    if (runtime.mapLoading) return runtime.mapLoading;
    if (runtime.mapSprite.visible) return Promise.resolve();
  }
  const request = Symbol(key);
  runtime.mapRequest = request;
  runtime.mapHref = key;
  runtime.mapSprite.visible = false;
  releaseMapTexture(runtime);
  if (!href) {
    runtime.mapLoading = undefined;
    drawMap(runtime);
    return Promise.resolve();
  }
  runtime.mapLoading = (async () => {
    let lease: EnhancedMapTextureLease | undefined;
    try {
      const original = await Assets.load<Texture>(href);
      if (runtime.disposed || runtime.mapRequest !== request) return;
      lease = await acquireEnhancedMapTexture(mapName, href);
      if (runtime.disposed || runtime.mapRequest !== request) return;
      runtime.mapSprite.texture = lease?.texture ?? original;
      runtime.mapLease = lease;
      lease = undefined;
      runtime.mapSprite.visible = true;
      drawMap(runtime);
    } catch (reason) {
      if (!runtime.disposed && runtime.mapRequest === request) {
        console.warn('Replay map image could not be loaded', reason);
      }
    } finally {
      // Includes unmount and A → B → A races while a texture is loading.
      lease?.release();
      if (runtime.mapRequest === request) runtime.mapLoading = undefined;
    }
  })();
  return runtime.mapLoading;
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

async function loadConsumableIcons(runtime: Runtime): Promise<void> {
  const keys = [...new Set((runtime.scene.consumables ?? []).map(consumableIconKey).filter((key): key is string=>Boolean(key)))];
  const loaded = await Promise.all(keys.map(async key => {
    const url = consumableIconUrl(key);
    if (!url) return undefined;
    try { return [key,await Assets.load<Texture>(url)] as const; }
    catch { return undefined; }
  }));
  if (runtime.disposed) return;
  runtime.consumableTextures = Object.fromEntries(loaded.filter((e): e is readonly [string,Texture]=>Boolean(e)));
  drawMap(runtime);
}

/** Nearest visible ship within 3.5% of the map width (last-known ships count), or undefined. */
export function pickShip(world: WorldPoint, ships: EvaluatedShip[], bounds: ReplayScene['map']['bounds']): EntityId | undefined {
  const worldRadius = (bounds.maxX - bounds.minX) * 0.035;
  const candidate = ships
    .filter((ship) => ship.knowledge !== 'hidden')
    .map((ship) => ({ ship, distance: Math.hypot(ship.displayPose.x - world.x, ship.displayPose.y - world.y) }))
    .filter(({ distance }) => distance <= worldRadius)
    .sort((left, right) => left.distance - right.distance)[0];
  return candidate?.ship.definition.id;
}

const SUPERSAMPLING = 2;

/** Draw the content into a 2x RenderTexture and show that. Resolution follows the stage. */
function renderSupersampled(runtime: Runtime): void {
  const { supersample } = runtime;
  if (!supersample) return;
  const resolution = runtime.deps.resolution * SUPERSAMPLING;
  const { texture } = supersample;
  if (texture && (texture.source.resolution !== resolution || texture.width !== runtime.width || texture.height !== runtime.height)) {
    supersample.sprite.texture = Texture.EMPTY;
    texture.destroy(true);
    supersample.texture = undefined;
  }
  if (!supersample.texture) {
    supersample.texture = RenderTexture.create({ width: runtime.width, height: runtime.height, resolution, antialias: true });
    supersample.sprite.texture = supersample.texture;
  }
  runtime.deps.renderer.render({ container: runtime.content, target: supersample.texture, clear: true });
}

export function createMapLayer(deps: MapLayerDeps, scene: ReplayScene): MapLayer {
  const container = new Container({ label: 'map' });
  const backdrop = new Graphics();
  const mapSprite = new Sprite(Texture.EMPTY);
  mapSprite.visible = false;
  const graphics = new Graphics();
  const markers = new Container();
  const content = new Container();
  const consumableGraphics = new Graphics({ label: 'consumable-ranges' });
  const consumableMask = new Graphics();
  consumableGraphics.mask = consumableMask;
  content.addChild(backdrop, mapSprite, consumableGraphics, consumableMask, graphics, markers);

  // Pooled texts start with this style; `styleMapText` sets the real one on first use.
  const initialSpec = mapTextSpec(11, '#ffffff');
  const disposeText = (text: Text) => text.destroy();
  const runtime: Runtime = {
    deps, content, backdrop, mapSprite, graphics, markers,
    consumableGraphics, consumableMask, consumableTextures: {},
    iconTextures: {}, powerupTextures: {}, planeTextures: {},
    scene, selectedShipId: '', presentation: 'broadcast',
    width: BOX.map.w, height: BOX.map.h,
    viewport: makeViewport(BOX.map.w, BOX.map.h),
    zoneLabels: new Pool(() => {
      const text = createMapText(deps, initialSpec);
      text.anchor.set(0.5);
      return text;
    }, text => text, disposeText),
    glyphs: new Pool(() => {
      const text = createMapText(deps, initialSpec);
      text.text = '✦';
      text.anchor.set(0.5);
      return text;
    }, text => text, disposeText),
    shipLabels: new Pool<ShipLabelEntry>(() => {
      const name = createMapText(deps, initialSpec);
      const root = new Container({ label: 'ship-name-label' });
      root.addChild(name);
      return { root, name };
    }, entry => entry.root, entry => entry.root.destroy({ children: true })),
  };
  if (deps.mapSupersampling) {
    const sprite = new Sprite({ label: 'map-supersampled' });
    runtime.supersample = { sprite };
    container.addChild(sprite);
  } else {
    container.addChild(content);
  }

  const hitArea = new Rectangle(0, 0, runtime.width, runtime.height);
  container.hitArea = hitArea;
  container.eventMode = 'static';
  let frameSelect: ((id: EntityId) => void) | undefined;
  container.on('pointerdown', (event: { clientX: number; clientY: number }) => {
    if (!runtime.latestState) return;
    const point = deps.toFrame(event.clientX, event.clientY);
    const id = pickShip(screenToWorld(runtime.scene, runtime.viewport, point), runtime.latestState.ships, runtime.scene.map.bounds);
    if (id) (frameSelect ?? deps.onSelect)(id);
  });

  const draw = (input: MapDrawInput) => {
    const sceneChanged = input.scene !== runtime.scene;
    runtime.scene = input.scene;
    runtime.latestState = input.state;
    runtime.selectedShipId = input.selectedId;
    runtime.presentation = input.presentation;
    runtime.width = input.width;
    runtime.height = input.height;
    hitArea.width = input.width;
    hitArea.height = input.height;
    drawMap(runtime);
    if (sceneChanged) {
      void loadMapImage(runtime);
      void loadPowerupIcons(runtime);
      void loadPlaneIcons(runtime);
    }
  };

  let ready: Promise<void> | undefined;
  return {
    container,
    ready() {
      ready ??= Promise.all([
        loadMapImage(runtime), loadShipIcons(runtime),
        loadPowerupIcons(runtime), loadPlaneIcons(runtime), loadConsumableIcons(runtime),
      ]).then(() => undefined);
      return ready;
    },
    draw,
    update(frame) {
      frameSelect = frame.selectShip;
      container.eventMode = frame.interactive ? 'static' : 'none';
      draw({ scene: frame.scene, state: frame.state, selectedId: frame.selectedId, presentation: 'broadcast', width: BOX.map.w, height: BOX.map.h });
    },
    destroy() {
      runtime.disposed = true;
      runtime.mapRequest = undefined;
      releaseMapTexture(runtime);
      runtime.zoneLabels.clear();
      runtime.glyphs.clear();
      runtime.shipLabels.clear();
      container.parent?.removeChild(container);
      container.destroy({ children: true });
      if (!runtime.supersample) return;
      runtime.supersample.texture?.destroy(true);
      content.destroy({ children: true });
    },
  };
}
