import { Assets, ColorMatrixFilter, type ColorMatrix, Container, Graphics, Sprite, type Text, type Texture, type TextStyleOptions } from 'pixi.js';
import { shipIconColor } from '../divisions';
import { formatNumber, selectedPanelModel, type PanelStat } from './intelModel';
import { BOX, COLORS, FONT_MONO } from './layout';
import { setTextIfChanged, truncateToWidth } from './text';
import type { BroadcastLayer, LayerFactory } from './types';

// Inner metrics of the `.player` section (final CSS of the former broadcast.css, see git history), in px
// relative to the panel box. Line boxes follow Space Grotesk / JetBrains Mono "normal" line heights.
const WIDTH = BOX.player.w;
const EYEBROW = { y: 2.6, lineHeight: 18 };
const NAME = { y: 23.6, lineHeight: 32 };
const SHIP_LINE = { y: 63.5, height: 23, iconBox: { w: 25, h: 20 }, gap: 10 };
/** `right:0` of the 771 px content box (the intel column is one px wider than BOX.player): frame x 1576. */
const SILHOUETTE = { x: WIDTH + 1 - 310, y: 8, w: 310, h: 84 };
/** Grid columns of `.stats` as `fr` shares, and the column gap. */
const OWNER_COLUMNS = [1.3, 1, 0.85];
const OTHER_COLUMNS = [0.8, 1.2, 1];
const STATS_GAP = 20;
/** Owner stats, measured in the DOM: values at frame y 136.9 (36 px) and 145.9 (21 px), labels at 176.9. */
const OWNER_STATS_Y = { values: [102.9, 111.9, 111.9], label: 142.9 };
const OTHER_STATS_Y = { values: [105.5, 105.5, 105.5], label: 145.5 };
const HP = { ownerY: 180.8, otherY: 179.5, lineHeight: 25, trackW: 350, trackH: 8, gap: 20 };
const HP_MAX_LABEL_COLOR = COLORS.muted;
const TRACK_COLOR = 0x253d30;
const HP_FILL_COLOR = COLORS.green;
/** CSS `brightness(0) invert(.85)` and `opacity:.85`: every opaque pixel becomes #d9d9d9 at alpha .85. */
const SILHOUETTE_GREY = 0.85;
const SILHOUETTE_ALPHA = 0.85;

const EYEBROW_STYLE: TextStyleOptions = { fontSize: 12, lineHeight: EYEBROW.lineHeight, letterSpacing: 1.5, fill: COLORS.gold };
const NAME_STYLE: TextStyleOptions = { fontSize: 29, fontWeight: '600', lineHeight: NAME.lineHeight, letterSpacing: -0.8, fill: COLORS.text };
const SHIP_STYLE: TextStyleOptions = { fontSize: 18, lineHeight: SHIP_LINE.height, fill: COLORS.gold };
const HP_STYLE: TextStyleOptions = { fontFamily: FONT_MONO, fontSize: 18, lineHeight: HP.lineHeight, fill: HP_MAX_LABEL_COLOR };

interface StatGroup { container: Container; values: Text[]; labels: Text[] }

function columnXs(shares: number[]): number[] {
  const unit = (WIDTH - STATS_GAP * (shares.length - 1)) / shares.reduce((a, b) => a + b, 0);
  const xs: number[] = [];
  let x = 0;
  for (const share of shares) { xs.push(x); x += share * unit + STATS_GAP; }
  return xs;
}

/** Re-assigns the truncated text only when the raw value changed. */
function fitter(text: Text, style: TextStyleOptions, maxWidth: number): (value: string) => void {
  let raw: string | undefined;
  return value => {
    if (value === raw) return;
    raw = value;
    setTextIfChanged(text, truncateToWidth(value, style, maxWidth));
  };
}

export const createPlayerPanelLayer: LayerFactory = (ctx, scene) => {
  const container = new Container({ label: 'player-panel' });
  container.position.set(BOX.player.x, BOX.player.y);

  const eyebrow = ctx.createText(EYEBROW_STYLE);
  eyebrow.position.set(0, EYEBROW.y);
  const name = ctx.createText(NAME_STYLE);
  name.position.set(0, NAME.y);
  const shipName = ctx.createText(SHIP_STYLE);
  shipName.position.set(SHIP_LINE.iconBox.w + SHIP_LINE.gap, SHIP_LINE.y);
  const iconSprites = new Map<string, Sprite>();
  container.addChild(eyebrow, name, shipName);
  const setEyebrow = fitter(eyebrow, EYEBROW_STYLE, WIDTH);
  const setName = fitter(name, NAME_STYLE, WIDTH);
  const setShipName = fitter(shipName, SHIP_STYLE, WIDTH - SHIP_LINE.iconBox.w - SHIP_LINE.gap);

  const makeGroup = (columns: number[], valueStyles: TextStyleOptions[], ys: { values: number[]; label: number }, labelStyle: TextStyleOptions): StatGroup => {
    const group: StatGroup = { container: new Container({ label: 'stats' }), values: [], labels: [] };
    columnXs(columns).forEach((x, i) => {
      const value = ctx.createText(valueStyles[i]);
      value.position.set(x, ys.values[i]);
      const label = ctx.createText(labelStyle);
      label.position.set(x, ys.label);
      group.values.push(value);
      group.labels.push(label);
      group.container.addChild(value, label);
    });
    container.addChild(group.container);
    return group;
  };
  const monoValue = (fontSize: number, lineHeight: number, extra: TextStyleOptions = {}): TextStyleOptions =>
    ({ fontFamily: FONT_MONO, fontSize, fontWeight: '500', lineHeight, letterSpacing: -0.8, fill: COLORS.text, ...extra });
  const ownerGroup = makeGroup(
    OWNER_COLUMNS,
    [monoValue(36, 36, { letterSpacing: -2, fill: '#f5f1df' }), monoValue(21, 28), monoValue(21, 28)],
    OWNER_STATS_Y,
    { fontSize: 12, lineHeight: 15, letterSpacing: 1.8, fill: '#9ab0a2' },
  );
  const otherGroup = makeGroup(
    OTHER_COLUMNS,
    [monoValue(21, 36), monoValue(21, 36), monoValue(21, 36)],
    OTHER_STATS_Y,
    { fontSize: 11, lineHeight: 14, letterSpacing: 1, fill: '#9ab0a2' },
  );
  const setStats = (group: StatGroup, stats: PanelStat[]) => stats.forEach((stat, i) => {
    setTextIfChanged(group.values[i], stat.value);
    setTextIfChanged(group.labels[i], stat.label);
  });

  const hp = new Container({ label: 'hp' });
  const track = new Graphics();
  const fill = new Graphics();
  const hpValue = ctx.createText({ ...HP_STYLE, fontWeight: '500', fill: HP_FILL_COLOR });
  const hpMax = ctx.createText(HP_STYLE);
  track.rect(0, (HP.lineHeight - HP.trackH) / 2, HP.trackW, HP.trackH).fill(TRACK_COLOR);
  hpValue.position.set(HP.trackW + HP.gap, 0);
  hpMax.position.set(HP.trackW + HP.gap, 0);
  hp.addChild(track, fill, hpValue, hpMax);
  container.addChild(hp);
  let drawnRatio = -1;

  let silhouette: Sprite | undefined;

  return {
    container,
    async ready() {
      if (!scene.ownerSilhouette) return;
      const texture = await Assets.load<Texture>(scene.ownerSilhouette);
      const sprite = new Sprite(texture);
      sprite.anchor.set(0.5);
      const scale = Math.min(SILHOUETTE.w / texture.width, SILHOUETTE.h / texture.height);
      sprite.scale.set(scale);
      sprite.position.set(SILHOUETTE.x + SILHOUETTE.w / 2, SILHOUETTE.y + SILHOUETTE.h / 2);
      sprite.filters = [silhouetteFilter()];
      sprite.visible = false;
      silhouette = sprite;
      // Behind the identity texts, like the CSS stacking (`.identity` z-index 1).
      container.addChildAt(sprite, 0);
    },
    update(frame) {
      const model = selectedPanelModel(frame);
      setEyebrow(model.eyebrow);
      setName(model.name);
      setShipName(model.shipName);

      const shipClass = model.definition.shipClass;
      let icon = iconSprites.get(shipClass);
      if (!icon) {
        icon = ctx.classIconSprite(shipClass);
        const scale = Math.min(SHIP_LINE.iconBox.w / icon.texture.width, SHIP_LINE.iconBox.h / icon.texture.height);
        icon.scale.set(scale);
        icon.position.set(SHIP_LINE.iconBox.w / 2, SHIP_LINE.y + SHIP_LINE.height / 2);
        iconSprites.set(shipClass, icon);
        container.addChild(icon);
      }
      for (const [key, sprite] of iconSprites) sprite.visible = key === shipClass;
      icon.tint = shipIconColor(model.definition, frame.scene.ships, frame.selectedId, COLORS.gold);

      if (silhouette) silhouette.visible = model.isOwner;
      ownerGroup.container.visible = model.isOwner;
      otherGroup.container.visible = !model.isOwner;
      setStats(model.isOwner ? ownerGroup : otherGroup, model.stats);

      hp.y = model.isOwner ? HP.ownerY : HP.otherY;
      if (model.healthRatio !== drawnRatio) {
        drawnRatio = model.healthRatio;
        fill.clear();
        if (model.healthRatio > 0) fill.rect(0, (HP.lineHeight - HP.trackH) / 2, HP.trackW * model.healthRatio, HP.trackH).fill(HP_FILL_COLOR);
      }
      setTextIfChanged(hpValue, formatNumber(model.health));
      setTextIfChanged(hpMax, ` / ${formatNumber(model.maxHealth)} HP`);
      hpMax.x = HP.trackW + HP.gap + hpValue.width;
    },
    destroy() { container.destroy({ children: true }); },
  } satisfies BroadcastLayer;
};

/**
 * R = G = B = grey and alpha x .85, set in the matrix: the result does not depend on how Pixi
 * applies the sprite alpha around a filter. Offsets are in 0..1; the shader un-premultiplies first.
 */
export function silhouetteMatrix(): ColorMatrix {
  const g = SILHOUETTE_GREY;
  return [
    0, 0, 0, 0, g,
    0, 0, 0, 0, g,
    0, 0, 0, 0, g,
    0, 0, 0, SILHOUETTE_ALPHA, 0,
  ];
}

function silhouetteFilter(): ColorMatrixFilter {
  const filter = new ColorMatrixFilter();
  filter.matrix = silhouetteMatrix();
  return filter;
}
