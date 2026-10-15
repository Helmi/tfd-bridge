import { Container, Graphics, type Text } from 'pixi.js';
import type { WorldPoint } from '../types';

type LabelOptions = {
  point: WorldPoint; viewport: {left: number; top: number; size: number};
  scale: number; color: string; ownDivision: boolean; alpha: number;
};

// Include the outline in the prefix width so the dot never touches the name.
const DOT_RADIUS = 2;
const DOT_OUTLINE = .8;
const DOT_SIZE = DOT_RADIUS * 2 + DOT_OUTLINE;
const DOT_GAP = 4;

export function shipLabelLayout(width: number, height: number, opts: LabelOptions) {
  const prefix = opts.ownDivision ? DOT_SIZE + DOT_GAP : 0;
  const totalWidth = width + prefix;
  const {point, viewport} = opts;
  return {
    x: Math.max(viewport.left, Math.min(viewport.left + viewport.size - totalWidth, point.x - totalWidth / 2)),
    y: Math.max(viewport.top, Math.min(viewport.top + viewport.size - height, point.y + 13 * opts.scale)),
    width: totalWidth, nameX: prefix, dotX: DOT_SIZE / 2, dotY: height / 2,
  };
}

/** Place an existing label (position, alpha, name offset) and sync its division dot. The caller styles `name`. */
export function updateShipLabel(label: Container, name: Text, opts: LabelOptions): void {
  name.anchor.set(0, 0);
  const layout = shipLabelLayout(name.width, name.height, opts);
  label.position.set(layout.x, layout.y);
  label.alpha = opts.alpha;
  name.position.set(layout.nameX, 0);
  let dot = label.getChildByLabel('division-name-dot') as Graphics | null;
  if (!opts.ownDivision) {
    if (dot) { label.removeChild(dot); dot.destroy(); }
    return;
  }
  if (!dot) {
    dot = new Graphics({label:'division-name-dot'});
    label.addChild(dot);
  }
  dot.clear().circle(layout.dotX, layout.dotY, DOT_RADIUS)
    .fill({color:opts.color}).stroke({color:'#06120e',width:DOT_OUTLINE});
}

export function createShipLabel(name: Text, opts: LabelOptions): Container {
  name.style.fill = opts.color;
  const label = new Container({label:'ship-name-label'});
  label.addChild(name);
  updateShipLabel(label, name, opts);
  return label;
}
