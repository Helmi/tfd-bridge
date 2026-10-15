import { Application, Text, type AccessibilitySystemOptions, type ApplicationOptions, type TextStyleOptions } from 'pixi.js';
import { compareFleetShips } from '../engine/fleetOrder';
import { evaluateScene } from '../engine/timeline';
import type { EntityId, EvaluatedShip, ReplayScene, Seconds } from '../types';
import { createLayers } from './createLayers';
import { loadBroadcastFonts } from './fonts';
import { createClassIconSprite, loadClassIcons, type ClassIconTextures } from './icons';
import { COLORS, FONT_SANS, FRAME, toFrame } from './layout';
import type { BroadcastFrameState, BroadcastLayer, StageContext } from './types';

export interface StageOptions {
  mode: 'interactive' | 'export';
  /** Initial renderer resolution: viewport scale x dpr (interactive) or output height / 1080 (export). */
  scale?: number;
  /** Export option for the map layer. Ignored in interactive mode. */
  mapSupersampling?: boolean;
  /** Per layer. Defaults to 60 seconds. */
  readyTimeoutMs?: number;
}

const READY_TIMEOUT_MS = 60_000;

function layerName(layer: BroadcastLayer, z: number): string {
  return layer.container.label || `layer@z${z}`;
}

function layerReady(layer: BroadcastLayer, name: string, timeoutMs: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`Layer "${name}" did not become ready within ${timeoutMs / 1000} seconds`)), timeoutMs);
    Promise.resolve().then(() => layer.ready?.()).then(
      () => { clearTimeout(timer); resolve(); },
      (error: unknown) => {
        clearTimeout(timer);
        reject(new Error(`Layer "${name}" failed to load: ${error instanceof Error ? error.message : String(error)}`, { cause: error }));
      },
    );
  });
}

/** Owns the Pixi Application and draws the 1920x1080 broadcast frame through a layer registry. */
export class BroadcastStage {
  private readonly layers: { layer: BroadcastLayer; z: number }[] = [];
  private icons!: ClassIconTextures;
  private selectHandler?: (id: EntityId) => void;
  private readonly reducedMotionQuery = typeof matchMedia === 'function' ? matchMedia('(prefers-reduced-motion: reduce)') : undefined;
  private destroyed = false;

  private constructor(private readonly app: Application, readonly mode: StageOptions['mode'], private readonly mapSupersampling: boolean) {}

  static async create(scene: ReplayScene, options: StageOptions): Promise<BroadcastStage> {
    const interactive = options.mode === 'interactive';
    const app = new Application();
    const init: Partial<ApplicationOptions> & AccessibilitySystemOptions = {
      width: FRAME.width,
      height: FRAME.height,
      resolution: options.scale ?? 1,
      autoDensity: true,
      antialias: true,
      backgroundColor: COLORS.frameBg,
      preference: 'webgl',
      autoStart: false,
      // Keyboard and screen reader support (Tab activates it) only for the live player.
      accessibilityOptions: { activateOnTab: interactive },
    };
    await app.init(init);
    const stage = new BroadcastStage(app, options.mode, interactive ? false : options.mapSupersampling ?? false);
    try {
      app.stage.sortableChildren = true;
      if (!interactive) app.stage.eventMode = 'none';
      await loadBroadcastFonts();
      stage.icons = await loadClassIcons();
      for (const { layer, z } of createLayers(stage.context(), scene)) stage.addLayer(layer, z);
      const timeout = options.readyTimeoutMs ?? READY_TIMEOUT_MS;
      await Promise.all(stage.layers.map(({ layer, z }) => layerReady(layer, layerName(layer, z), timeout)));
      return stage;
    } catch (error) {
      stage.destroy();
      throw error;
    }
  }

  get canvas(): HTMLCanvasElement { return this.app.canvas; }
  get resolution(): number { return this.app.renderer.resolution; }

  /** Called by the interactive player when a layer selects a ship. Export ignores it. */
  set onSelectShip(handler: ((id: EntityId) => void) | undefined) { this.selectHandler = handler; }

  addLayer(layer: BroadcastLayer, z: number): void {
    layer.container.zIndex = z;
    this.app.stage.addChild(layer.container);
    this.layers.push({ layer, z });
    this.layers.sort((a, b) => a.z - b.z);
  }

  /** Change the renderer resolution (viewport resize, dpr change). Call `render()` afterwards. */
  setResolution(resolution: number): void {
    if (resolution === this.resolution) return;
    this.app.renderer.resize(FRAME.width, FRAME.height, resolution);
  }

  update(scene: ReplayScene, time: Seconds, selectedId: EntityId): void {
    const state = evaluateScene(scene, Math.min(time, scene.replay.duration));
    const owner = state.ships.find(s => s.definition.id === scene.replay.perspectiveEntityId)
      ?? state.ships.find(s => s.definition.relation === 'self')
      ?? state.ships[0];
    const ownerTeamId = owner.definition.teamId;
    const order = (a: EvaluatedShip, b: EvaluatedShip) => compareFleetShips(a.definition, b.definition);
    const interactive = this.mode === 'interactive';
    const frame: BroadcastFrameState = {
      scene, time, state, selectedId,
      ownerId: owner.definition.id,
      ownerTeamId,
      ally: state.ships.filter(s => s.definition.teamId === ownerTeamId).sort(order),
      enemy: state.ships.filter(s => s.definition.teamId !== ownerTeamId).sort(order),
      interactive,
      reducedMotion: interactive && Boolean(this.reducedMotionQuery?.matches),
      selectShip: id => { if (interactive) this.selectHandler?.(id); },
    };
    for (const { layer } of this.layers) layer.update(frame);
  }

  render(): void {
    this.app.render();
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    for (const { layer } of this.layers) layer.destroy();
    this.layers.length = 0;
    this.app.destroy({ removeView: true }, { children: true });
  }

  private context(): StageContext {
    const stage = this;
    return {
      mode: this.mode,
      get renderer() { return stage.app.renderer; },
      get resolution() { return stage.resolution; },
      mapSupersampling: this.mapSupersampling,
      toFrame: (clientX, clientY) => toFrame(stage.app.canvas.getBoundingClientRect(), clientX, clientY),
      createText: (style: TextStyleOptions) => new Text({ style: { fontFamily: FONT_SANS, ...style } }),
      classIconSprite: shipClass => createClassIconSprite(stage.icons, shipClass),
    };
  }
}
