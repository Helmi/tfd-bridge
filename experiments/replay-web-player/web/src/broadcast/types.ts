import type { Renderer, Container, Sprite, Text, TextStyleOptions } from 'pixi.js';
import type { EntityId, EvaluatedShip, ReplayScene, SceneState, Seconds, TeamId } from '../types';

export interface BroadcastFrameState {
  scene: ReplayScene;
  /** Raw time. It runs past `scene.replay.duration` during the ending. */
  time: Seconds;
  /** `evaluateScene(scene, min(time, duration))`, computed once per tick by the stage. */
  state: SceneState;
  selectedId: EntityId;
  ownerId: EntityId;
  ownerTeamId: TeamId;
  /** Split by team against the owner, each sorted with `compareFleetShips`. */
  ally: EvaluatedShip[];
  enemy: EvaluatedShip[];
  /** False in export: no hit areas, no accessibility. */
  interactive: boolean;
  reducedMotion: boolean;
  /** No-op in export. */
  selectShip(id: EntityId): void;
}

/** Created by the stage and passed to every layer factory. */
export interface StageContext {
  readonly mode: 'interactive' | 'export';
  /** For example for a RenderTexture. */
  readonly renderer: Renderer;
  /** Current renderer resolution. It changes on resize and dpr change. */
  readonly resolution: number;
  /** Export option, false in interactive mode. */
  readonly mapSupersampling: boolean;
  toFrame(clientX: number, clientY: number): { x: number; y: number };
  /**
   * Draw the frame now. The interactive stage only renders when the player ticks, so a layer that
   * changes while playback is paused (the keyboard focus ring) asks for a redraw. No-op in export.
   */
  requestRender(): void;
  /** Resolution follows the stage. */
  createText(style: TextStyleOptions): Text;
  /** Texture loaded once per stage, rotated 90 degrees. The caller sets the tint. */
  classIconSprite(shipClass: string): Sprite;
}

export interface BroadcastLayer {
  /** Set `container.label` to a short name: the stage reports it when `ready()` fails. */
  readonly container: Container;
  /** Async assets. The stage awaits all of them before the first render. */
  ready?(): Promise<void>;
  update(frame: BroadcastFrameState): void;
  destroy(): void;
}

export type LayerFactory = (ctx: StageContext, scene: ReplayScene) => BroadcastLayer;
