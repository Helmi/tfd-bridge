import { BroadcastStage } from '../broadcast/BroadcastStage';
import { backingSize } from '../broadcast/layout';
import type { ReplayScene } from '../types';

/** What `encodeMp4.ts` needs from a frame renderer. */
export interface FrameRenderer {
  readonly canvas: HTMLCanvasElement;
  readonly width: number;
  readonly height: number;
  renderFrame(time: number): Promise<void>;
  destroy(): void;
}

/** Export renderer: one Pixi broadcast stage, drawn straight to its WebGL canvas. */
export class PixiFrameRenderer implements FrameRenderer {
  readonly width: number;
  readonly height: number;

  private constructor(private readonly stage: BroadcastStage, private readonly scene: ReplayScene, private readonly selectedId: string, scale: number) {
    ({ width: this.width, height: this.height } = backingSize(scale));
  }

  static async create(scene: ReplayScene, opts: { scale?: number; mapSupersampling?: boolean } = {}): Promise<PixiFrameRenderer> {
    const scale = opts.scale ?? 1;
    const stage = await BroadcastStage.create(scene, { mode: 'export', scale, mapSupersampling: opts.mapSupersampling ?? false });
    const selectedId = scene.replay.perspectiveEntityId ?? scene.ships.find(s => s.relation === 'self')?.id ?? scene.ships[0].id;
    return new PixiFrameRenderer(stage, scene, selectedId, scale);
  }

  get canvas(): HTMLCanvasElement { return this.stage.canvas; }

  /** Read `canvas` synchronously after this resolves: the WebGL buffer is not preserved. */
  async renderFrame(time: number): Promise<void> {
    this.stage.update(this.scene, time, this.selectedId);
    this.stage.render();
  }

  destroy(): void { this.stage.destroy(); }
}
