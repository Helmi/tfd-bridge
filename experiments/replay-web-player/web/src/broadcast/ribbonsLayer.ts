import { Assets, Container, Graphics, Sprite, type Text, type Texture, type TextStyleOptions } from 'pixi.js';
import { consumableList, ribbonList, type ConsumableItem, type RibbonItem } from './intelModel';
import { BOX, COLORS, FONT_MONO } from './layout';
import { setTextIfChanged, truncateToWidth } from './text';
import type { BroadcastLayer, LayerFactory } from './types';

// Inner metrics of the `.ribbons` section (final CSS in components/broadcast.css), in px
// relative to the strip box: 1 px top border, 18 px padding, then a 42 px content row.
const WIDTH = BOX.ribbons.w;
const CONTENT_Y = 19;
const ROW_HEIGHT = 42;

const BADGE = { w: 108, h: 42, gap: 12 };
const FALLBACK_STYLE: TextStyleOptions = { fontSize: 10, fill: COLORS.text };
const COUNT = { border: 0x718272, borderAlpha: 0x77 / 255, background: 0x07110f, backgroundAlpha: 0xee / 255, text: '#f1f5ec', radius: 3 };

const CHIP = { padX: 10, padY: 7, border: 1, textHeight: 20.4, gap: 10, secondsGap: 8, secondsOffsetY: 2.5, color: 0x344d40, radius: 4 };
const CHIP_STYLE: TextStyleOptions = { fontSize: 16, lineHeight: CHIP.textHeight, fill: COLORS.text };
const SECONDS_STYLE: TextStyleOptions = { fontFamily: FONT_MONO, fontSize: 13, fontWeight: '500', lineHeight: 17, fill: COLORS.gold };
const EMPTY_STYLE: TextStyleOptions = { fontSize: 14, lineHeight: 18, fill: COLORS.muted };

const clamp = (min: number, value: number, max: number) => Math.min(max, Math.max(min, value));

interface Badge {
  container: Container;
  image: Sprite;
  fallback: Text;
  box: Graphics;
  count: Text;
  /** Last drawn badge width and count, to skip redundant work. */
  signature?: string;
  fallbackKey?: string;
}

export const createRibbonsLayer: LayerFactory = (ctx, scene) => {
  const container = new Container({ label: 'ribbons' });
  container.position.set(BOX.ribbons.x, BOX.ribbons.y);
  container.addChild(new Graphics().rect(0, 0, WIDTH, 1).fill(COLORS.line));

  const textures = new Map<string, Texture>();
  const ribbonRow = new Container({ label: 'ribbon-list' });
  const badges = new Map<string, Badge>();
  const chipRow = new Container({ label: 'consumables' });
  const chipBorders = new Graphics();
  const chipNames: Text[] = [];
  const chipSeconds: Text[] = [];
  const empty = ctx.createText(EMPTY_STYLE);
  empty.position.set(0, CONTENT_Y + (ROW_HEIGHT - EMPTY_STYLE.lineHeight!) / 2);
  setTextIfChanged(empty, 'No active consumables observed');
  chipRow.addChild(chipBorders, empty);
  container.addChild(ribbonRow, chipRow);
  let chipSignature = '';

  function badgeFor(item: RibbonItem): Badge {
    let badge = badges.get(item.key);
    if (!badge) {
      const badgeContainer = new Container({ label: `ribbon:${item.key}` });
      const image = new Sprite();
      image.anchor.set(0.5);
      const fallback = ctx.createText(FALLBACK_STYLE);
      const box = new Graphics();
      const count = ctx.createText({ fontSize: 17, fontWeight: '700', fill: COUNT.text });
      badgeContainer.addChild(image, fallback, box, count);
      ribbonRow.addChild(badgeContainer);
      badge = { container: badgeContainer, image, fallback, box, count };
      badges.set(item.key, badge);
    }
    return badge;
  }

  function drawBadge(badge: Badge, item: RibbonItem, width: number, height: number): void {
    const texture = item.imageUrl ? textures.get(item.imageUrl) : undefined;
    badge.image.visible = Boolean(texture);
    badge.fallback.visible = !texture;
    if (texture) {
      if (badge.image.texture !== texture) badge.image.texture = texture;
      badge.image.scale.set(Math.min(width / texture.width, height / texture.height));
      badge.image.position.set(width / 2, height / 2);
    } else {
      const key = `${item.label}|${width}`;
      if (badge.fallbackKey !== key) {
        badge.fallbackKey = key;
        setTextIfChanged(badge.fallback, truncateToWidth(item.label, FALLBACK_STYLE, width));
      }
    }

    const signature = `${width}|${item.count}`;
    if (badge.signature === signature) return;
    badge.signature = signature;
    // Mirrors the CSS `cqw` rules: 1 cqw is 1 % of the badge width.
    const cq = width / 100;
    const fontSize = clamp(11, 15.74 * cq, 17);
    const padX = clamp(2, 4.63 * cq, 5);
    const padY = clamp(1, 1.85 * cq, 2);
    const lineHeight = fontSize * 1.18;
    badge.count.style.fontSize = fontSize;
    badge.count.style.lineHeight = lineHeight;
    setTextIfChanged(badge.count, String(item.count));
    const boxW = Math.max(clamp(18, 25 * cq, 27), badge.count.width + 2 * padX + 2);
    const boxH = lineHeight + 2 * padY + 2;
    const boxX = width - boxW;
    const boxY = height - clamp(-3, -2.78 * cq, -1) - boxH;
    badge.box.clear()
      .roundRect(boxX, boxY, boxW, boxH, COUNT.radius).fill({ color: COUNT.background, alpha: COUNT.backgroundAlpha })
      .roundRect(boxX + 0.5, boxY + 0.5, boxW - 1, boxH - 1, COUNT.radius).stroke({ width: 1, color: COUNT.border, alpha: COUNT.borderAlpha });
    badge.count.position.set(boxX + (boxW - badge.count.width) / 2, boxY + 1 + padY);
  }

  function updateRibbons(items: RibbonItem[]): void {
    for (const badge of badges.values()) badge.container.visible = false;
    if (!items.length) return;
    // `flex: 0 1 108px`: all badges shrink equally when the row is too wide.
    const width = Math.min(BADGE.w, (WIDTH - (items.length - 1) * BADGE.gap) / items.length);
    const height = width * BADGE.h / BADGE.w;
    items.forEach((item, index) => {
      const badge = badgeFor(item);
      badge.container.visible = true;
      badge.container.position.set(index * (width + BADGE.gap), CONTENT_Y + (ROW_HEIGHT - height) / 2);
      drawBadge(badge, item, width, height);
    });
  }

  function chipText(pool: Text[], index: number, style: TextStyleOptions): Text {
    if (!pool[index]) {
      pool[index] = ctx.createText(style);
      chipRow.addChild(pool[index]);
    }
    return pool[index];
  }

  function updateChips(items: ConsumableItem[]): void {
    empty.visible = items.length === 0;
    const chips = items.map((item, index) => {
      const name = chipText(chipNames, index, CHIP_STYLE);
      const seconds = chipText(chipSeconds, index, SECONDS_STYLE);
      // The trailing space is the literal space before the <b> in the DOM version.
      setTextIfChanged(name, `${item.label} `);
      setTextIfChanged(seconds, `${item.seconds}s`);
      name.visible = seconds.visible = true;
      return { name, seconds, width: 2 * CHIP.border + 2 * CHIP.padX + name.width + CHIP.secondsGap + seconds.width };
    });
    for (let i = items.length; i < chipNames.length; i++) chipNames[i].visible = chipSeconds[i].visible = false;

    const height = CHIP.textHeight + 2 * CHIP.padY + 2 * CHIP.border;
    // Wrapping flex row: a single row is centered in the 42 px content row.
    const rows: number[][] = [];
    let x = 0;
    chips.forEach((chip, index) => {
      if (x > 0 && x + chip.width > WIDTH) { rows.push([]); x = 0; }
      if (!rows.length) rows.push([]);
      rows[rows.length - 1].push(index);
      x += chip.width + CHIP.gap;
    });
    const contentHeight = rows.length * height + Math.max(0, rows.length - 1) * CHIP.gap;
    const top = CONTENT_Y + Math.max(0, (ROW_HEIGHT - contentHeight) / 2);
    const placed: { x: number; y: number; width: number }[] = [];
    rows.forEach((row, rowIndex) => {
      let left = 0;
      for (const index of row) {
        placed[index] = { x: left, y: top + rowIndex * (height + CHIP.gap), width: chips[index].width };
        left += chips[index].width + CHIP.gap;
      }
    });
    placed.forEach(({ x: left, y }, index) => {
      const { name, seconds } = chips[index];
      const contentX = left + CHIP.border + CHIP.padX;
      name.position.set(contentX, y + CHIP.border + CHIP.padY);
      seconds.position.set(contentX + name.width + CHIP.secondsGap, y + CHIP.border + CHIP.padY + CHIP.secondsOffsetY);
    });

    const signature = placed.map(p => `${p.x},${p.y},${p.width}`).join(';');
    if (signature === chipSignature) return;
    chipSignature = signature;
    chipBorders.clear();
    for (const { x: left, y, width } of placed) {
      chipBorders.roundRect(left + 0.5, y + 0.5, width - 1, height - 1, CHIP.radius).stroke({ width: CHIP.border, color: CHIP.color });
    }
  }

  return {
    container,
    async ready() {
      const urls = new Set<string>();
      for (const definition of Object.values(scene.ribbonDefinitions ?? {})) {
        if (definition.imageUrl && !definition.isSubribbon) urls.add(definition.imageUrl);
      }
      await Promise.all([...urls].map(async url => { textures.set(url, await Assets.load<Texture>(url)); }));
    },
    update(frame) {
      const { state, selectedId, ownerId } = frame;
      // An unknown selection falls back to the owner, like the panel.
      const ownerSelected = selectedId === ownerId || !state.ships.some(s => s.definition.id === selectedId);
      ribbonRow.visible = ownerSelected;
      chipRow.visible = !ownerSelected;
      if (ownerSelected) updateRibbons(ribbonList(state.ownerStats, frame.scene.ribbonDefinitions));
      else updateChips(consumableList(state.consumables, selectedId));
    },
    destroy() { container.destroy({ children: true }); },
  } satisfies BroadcastLayer;
};
