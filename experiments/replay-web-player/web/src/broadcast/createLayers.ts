import { Container, Graphics } from 'pixi.js';
import { createFeedLayer } from './feedLayer';
import { COLORS, FRAME, Z } from './layout';
import { createScoreboardLayer } from './scoreboardLayer';
import { createRosterLayer } from './rosterLayer';
import { createPlayerPanelLayer } from './playerPanelLayer';
import { createRibbonsLayer } from './ribbonsLayer';
import type { BroadcastLayer, LayerFactory, StageContext } from './types';
import { createMapLayer, mapLayerDeps } from './mapLayer';
import type { ReplayScene } from '../types';

export interface LayerEntry { layer: BroadcastLayer; z: number }

const createBackgroundLayer: LayerFactory = () => {
  const container = new Container({ label: 'background' });
  container.addChild(new Graphics().rect(0, 0, FRAME.width, FRAME.height).fill(COLORS.frameBg));
  return { container, update() {}, destroy() { container.destroy({ children: true }); } };
};

/** One `stage.addLayer(...)` line per layer. The stage adds them in this order. */
export function createLayers(ctx: StageContext, scene: ReplayScene): LayerEntry[] {
  const entries: LayerEntry[] = [];
  const stage = { addLayer: (layer: BroadcastLayer, z: number) => { entries.push({ layer, z }); } };
  stage.addLayer(createBackgroundLayer(ctx, scene), Z.BACKGROUND);
  stage.addLayer(createScoreboardLayer(ctx, scene), Z.SCOREBOARD);
  stage.addLayer(createRosterLayer(ctx, scene), Z.PANEL);
  stage.addLayer(createFeedLayer(ctx, scene), Z.PANEL);
  stage.addLayer(createMapLayer(mapLayerDeps(ctx), scene), Z.MAP);
  stage.addLayer(createPlayerPanelLayer(ctx, scene), Z.PANEL);
  stage.addLayer(createRibbonsLayer(ctx, scene), Z.PANEL);
  return entries;
}
