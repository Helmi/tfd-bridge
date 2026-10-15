import { Container, Graphics, type Sprite, type Text } from 'pixi.js';
import { BOX, COLORS, FONT_MONO, SCOREBOARD_BG_ALPHA } from './layout';
import { buildScoreboardModel, type ScoreboardCap, type ScoreboardShip, type ScoreboardSide } from './scoreboardModel';
import { setTextIfChanged } from './text';
import type { BroadcastFrameState, BroadcastLayer, LayerFactory } from './types';

// Inner metrics: the last matching `.scoreboard` rules in components/broadcast.css.
const PADDING = { top: 12, side: 10 };
const CENTER_WIDTH = 160;
const SCORE_FONT_SIZE = 44;
const ICON_SLOT = { w: 27, h: 25 };
const ICON_GAP = 3;
const SCORE_ICON_GAP = 10;
const DEAD_ICON = { color: '#68766d', alpha: 0.45 };
const MAP_LABEL = { y: 10, color: '#9bb3a3' };
const CLOCK = { y: 37.25, color: '#d9e4db' };
const CAP = { y: 65, diameter: 21, gap: 9 };

const TEAM_COLORS = { friendly: COLORS.green, enemy: COLORS.red };
const capColor = (owner: ScoreboardSide | null) => (owner ? TEAM_COLORS[owner] : CLOCK.color);

/** Frame x of the centre of the left column, the clock column and the right column. */
const innerWidth = BOX.scoreboard.w - 2 * PADDING.side;
const sideWidth = (innerWidth - CENTER_WIDTH) / 2;
const COLUMN_X = {
  friendly: BOX.scoreboard.x + PADDING.side + sideWidth / 2,
  center: BOX.scoreboard.x + BOX.scoreboard.w / 2,
  enemy: BOX.scoreboard.x + BOX.scoreboard.w - PADDING.side - sideWidth / 2,
};

interface Strip { key: string; sprites: Sprite[]; state: string[] }

export const createScoreboardLayer: LayerFactory = (ctx, scene) => {
  const container = new Container({ label: 'scoreboard' });
  const { x, y, w, h } = BOX.scoreboard;
  container.addChild(new Graphics().roundRect(x, y, w, h, 4).fill({ color: COLORS.scoreboardBg, alpha: SCOREBOARD_BG_ALPHA }));

  const scoreText = (side: ScoreboardSide) => {
    const text = ctx.createText({
      fontFamily: FONT_MONO, fontSize: SCORE_FONT_SIZE, fontWeight: '600', lineHeight: SCORE_FONT_SIZE, letterSpacing: -2,
      fill: TEAM_COLORS[side],
      dropShadow: { color: '#000000', alpha: 0.53, blur: 4, distance: 2, angle: Math.PI / 2 },
    });
    text.anchor.set(0.5, 0);
    text.position.set(COLUMN_X[side], y + PADDING.top);
    container.addChild(text);
    return text;
  };
  const centerText = (offsetY: number, style: Parameters<typeof ctx.createText>[0]) => {
    const text = ctx.createText(style);
    text.anchor.set(0.5);
    text.position.set(COLUMN_X.center, y + PADDING.top + offsetY);
    container.addChild(text);
    return text;
  };
  const friendlyScore = scoreText('friendly');
  const enemyScore = scoreText('enemy');
  const mapLabel = centerText(MAP_LABEL.y, { fontSize: 11, letterSpacing: 2, fill: MAP_LABEL.color });
  const clockText = centerText(CLOCK.y, { fontFamily: FONT_MONO, fontSize: 23, fontWeight: '500', fill: CLOCK.color });

  // One ring and one letter per capture zone, centred as a row under the clock.
  const capsRing = new Graphics();
  container.addChild(capsRing);
  const rowWidth = scene.captureZones.length * CAP.diameter + Math.max(0, scene.captureZones.length - 1) * CAP.gap;
  const capTexts: Text[] = scene.captureZones.map((_, index) => {
    const text = ctx.createText({ fontSize: 14, fontWeight: '600', fill: CLOCK.color });
    text.anchor.set(0.5);
    text.position.set(COLUMN_X.center - rowWidth / 2 + CAP.diameter / 2 + index * (CAP.diameter + CAP.gap), y + PADDING.top + CAP.y);
    container.addChild(text);
    return text;
  });
  let capsKey = '';

  const strips: Record<ScoreboardSide, Strip> = {
    friendly: { key: '', sprites: [], state: [] },
    enemy: { key: '', sprites: [], state: [] },
  };

  /** Icons are created once per strip and again only if the ship list changes. */
  function updateStrip(side: ScoreboardSide, ships: ScoreboardShip[]): void {
    const strip = strips[side];
    const key = ships.map(s => `${s.id}:${s.shipClass}`).join('|');
    if (key !== strip.key) {
      for (const sprite of strip.sprites) sprite.destroy();
      const pitch = ICON_SLOT.w + ICON_GAP;
      const left = COLUMN_X[side] - (ships.length * pitch - ICON_GAP) / 2;
      const centerY = y + PADDING.top + SCORE_FONT_SIZE + SCORE_ICON_GAP + ICON_SLOT.h / 2;
      strip.sprites = ships.map((ship, index) => {
        const sprite = ctx.classIconSprite(ship.shipClass);
        // The DOM icon is "contain" in its 27x25 slot, then rotated.
        sprite.scale.set(Math.min(ICON_SLOT.w / sprite.texture.width, ICON_SLOT.h / sprite.texture.height));
        sprite.position.set(left + ICON_SLOT.w / 2 + index * pitch, centerY);
        container.addChild(sprite);
        return sprite;
      });
      strip.state = [];
      strip.key = key;
    }
    ships.forEach((ship, index) => {
      const state = ship.dead ? 'dead' : ship.color;
      if (strip.state[index] === state) return;
      strip.state[index] = state;
      strip.sprites[index].tint = ship.dead ? DEAD_ICON.color : ship.color;
      strip.sprites[index].alpha = ship.dead ? DEAD_ICON.alpha : 1;
    });
  }

  function updateCaps(caps: ScoreboardCap[]): void {
    caps.forEach((cap, index) => setTextIfChanged(capTexts[index], cap.label));
    const key = caps.map(c => c.owner ?? '-').join();
    if (key === capsKey) return;
    capsKey = key;
    capsRing.clear();
    caps.forEach((cap, index) => {
      const color = capColor(cap.owner);
      capTexts[index].style.fill = color;
      const cx = COLUMN_X.center - rowWidth / 2 + CAP.diameter / 2 + index * (CAP.diameter + CAP.gap);
      capsRing.circle(cx, y + PADDING.top + CAP.y, (CAP.diameter - 1) / 2).stroke({ width: 1, color });
    });
  }

  const layer: BroadcastLayer = {
    container,
    update(frame: BroadcastFrameState) {
      const model = buildScoreboardModel(frame, TEAM_COLORS);
      setTextIfChanged(friendlyScore, String(model.friendlyScore));
      setTextIfChanged(enemyScore, String(model.enemyScore));
      setTextIfChanged(mapLabel, model.mapLabel);
      setTextIfChanged(clockText, model.clock);
      updateStrip('friendly', model.friendlyShips);
      updateStrip('enemy', model.enemyShips);
      updateCaps(model.caps);
    },
    destroy() { container.destroy({ children: true }); },
  };
  return layer;
};
