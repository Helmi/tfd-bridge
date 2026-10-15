import { CanvasTextMetrics, Container, Graphics, Rectangle, Sprite, TextStyle, type Text, type TextStyleOptions, type Texture } from 'pixi.js';
import { chatChannelColor } from '../chatColors';
import { clock } from '../time';
import type { ShipDefinition } from '../types';
import {
  buildFeedEvents, ENTRANCE_SECONDS, feedEntranceRemaining, ROW_GAP, VIEWPORT_ROWS, type FeedEvent,
} from './feedModel';
import { layoutInline, type InlineLayout, type InlineToken } from './inlineLayout';
import { BOX, COLORS, FEED_WINDOW_HEIGHT, FONT_MONO, FONT_SANS } from './layout';
import { setTextIfChanged, type MeasureText } from './text';
import type { BroadcastFrameState, BroadcastLayer, StageContext } from './types';
import type { ReplayScene } from '../types';

const LINE_HEIGHT = 22;
const MIN_ROW_HEIGHT = 24;
const TIME_WIDTH = 33;
const ICON_WIDTH = 16;
const COLUMN_GAP = 12;
const CONTENT_X = TIME_WIDTH + ICON_WIDTH + 2 * COLUMN_GAP;
const CONTENT_WIDTH = BOX.feed.w - CONTENT_X;
const CLASS_ICON_SIZE = 16;
const CLASS_ICON_GAP = 3;
const VERB_MARGIN = 7;
const CHAT_MARGIN = 12;
const GAP_ICON = 'gap';
const KNOWN_CLASSES = ['carrier', 'battleship', 'cruiser', 'destroyer', 'submarine'];

const CHAT_ICON_COLOR = '#9eaf9f';
const KILL_ICON_COLOR = COLORS.red;
const HANDLE_COLOR = '#b8c2bc';
// Official Lucide ChevronRight and X paths. See public/assets/lucide/LICENSE.
const ICON_PATHS = { chat: ['m9 18 6-6-6-6'], kill: ['M18 6 6 18', 'm6 6 12 12'] } as const;

const measureWithPixi: MeasureText = (value, style) => CanvasTextMetrics.measureText(value, new TextStyle(style)).width;

const cache = new Map<string, TextStyleOptions>();
/** One shared style object per look, so adjacent runs of the same look merge into one Text. */
const cached = (fontSize: number, fontWeight: '400' | '500', fill: string): TextStyleOptions => {
  const key = `${fontSize}|${fontWeight}|${fill}`;
  let value = cache.get(key);
  if (!value) { value = { fontFamily: FONT_SANS, fontSize, fontWeight, fill, lineHeight: LINE_HEIGHT }; cache.set(key, value); }
  return value;
};

const TIME_STYLE: TextStyleOptions = { fontFamily: FONT_MONO, fontSize: 11, fontWeight: '400', fill: '#83988b', lineHeight: LINE_HEIGHT };
const EMPTY_STYLE = cached(15, '400', COLORS.muted);

export const FEED_EMPTY_TEXT = 'Waiting for the first battle event…';

/** Arrived events, newest `VIEWPORT_ROWS` plus the entering ones, oldest first. `events` is newest first. */
export function visibleFeedEvents(events: FeedEvent[], time: number): FeedEvent[] {
  const arrived = events.filter(e => e.t <= time);
  const entering = arrived.filter(e => time - e.t < ENTRANCE_SECONDS).length;
  return arrived.slice(0, VIEWPORT_ROWS + entering).reverse();
}

export const feedRowHeight = (lineCount: number): number => Math.max(MIN_ROW_HEIGHT, lineCount * LINE_HEIGHT);

/** Downward shift of the whole stack while rows enter. Zero with reduced motion. */
export function feedStackOffset(rows: { t: number; height: number }[], time: number, reducedMotion: boolean): number {
  if (reducedMotion) return 0;
  return rows.reduce((sum, row) => sum + (row.height + ROW_GAP) * feedEntranceRemaining(time - row.t), 0);
}

function participantTokens(ship: ShipDefinition | undefined, ownerTeamId: string, fallback?: string): InlineToken[] {
  const handle = cached(14, '400', HANDLE_COLOR);
  if (!ship) return fallback
    ? [{ text: fallback, style: handle }]
    : [{ text: 'Unknown', style: cached(15, '500', HANDLE_COLOR) }];
  const color = ship.teamId === ownerTeamId ? COLORS.green : COLORS.red;
  const tokens: InlineToken[] = [];
  // Runtime guard: old/custom scene files may contain an unrecognised class.
  if (KNOWN_CLASSES.includes(ship.shipClass)) tokens.push({ icon: `class:${ship.shipClass}:${color}`, width: CLASS_ICON_SIZE + CLASS_ICON_GAP });
  // The ship label is one unit in the DOM, so its spaces do not break.
  tokens.push({ text: ship.shipName.replace(/ /g, '\u00a0'), style: cached(15, '500', color) });
  if (ship.playerName) {
    tokens.push({ text: ' ', style: cached(15, '400', color) });
    tokens.push({ text: `| ${ship.clan ? `[${ship.clan}] ` : ''}${ship.playerName}`, style: handle });
  }
  return tokens;
}

export function feedRowTokens(event: FeedEvent, ownerTeamId: string): InlineToken[] {
  const sender = participantTokens(event.sender, ownerTeamId, event.kind === 'chat' ? event.senderName || 'Unknown' : undefined);
  if (event.kind === 'chat') {
    return [...sender, { icon: GAP_ICON, width: CHAT_MARGIN }, { text: event.message ?? '', style: cached(15, '400', event.color ?? chatChannelColor('')) }];
  }
  return [
    ...sender,
    { icon: GAP_ICON, width: VERB_MARGIN }, { text: ' sunk ', style: cached(13, '400', '#8ca296') }, { icon: GAP_ICON, width: VERB_MARGIN },
    ...participantTokens(event.target, ownerTeamId),
  ];
}

export interface FeedRowLayout { layout: InlineLayout; height: number }

export function layoutFeedRow(event: FeedEvent, ownerTeamId: string, measure: MeasureText): FeedRowLayout {
  const layout = layoutInline(feedRowTokens(event, ownerTeamId), CONTENT_WIDTH, measure);
  return { layout, height: feedRowHeight(layout.lineCount) };
}

interface RowView { container: Container; height: number }

function createEventTextures(ctx: StageContext): Record<'chat' | 'kill', Texture> {
  const make = (kind: 'chat' | 'kill') => {
    const art = new Graphics().svg(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">${
      ICON_PATHS[kind].map(d => `<path d="${d}" fill="none" stroke="#ffffff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>`).join('')
    }</svg>`);
    const holder = new Container();
    // Scale the child: the target's own transform is not part of generateTexture's contract.
    art.scale.set(ICON_WIDTH / 24);
    holder.addChild(art);
    const texture = ctx.renderer.generateTexture({ target: holder, frame: new Rectangle(0, 0, ICON_WIDTH, ICON_WIDTH), resolution: Math.max(2, Math.ceil(ctx.resolution)) });
    holder.destroy({ children: true });
    return texture;
  };
  return { chat: make('chat'), kill: make('kill') };
}

export function createFeedLayer(ctx: StageContext, scene: ReplayScene, measure: MeasureText = measureWithPixi): BroadcastLayer {
  const root = new Container({ label: 'feed' });
  // `.feed{border-top}`: 1 px, spanning the 771 px content box of the intel column.
  root.addChild(new Graphics().rect(BOX.feed.x, BOX.feed.y, BOX.feed.w + 1, 1).fill(COLORS.line));
  const container = new Container({ label: 'feed-window' });
  root.addChild(container);
  container.position.set(BOX.feedWindow.x, BOX.feedWindow.y);
  const mask = new Graphics().rect(0, 0, BOX.feedWindow.w, FEED_WINDOW_HEIGHT).fill(0xffffff);
  container.addChild(mask);
  container.mask = mask;
  const stack = new Container({ label: 'feed-rows' });
  container.addChild(stack);
  const empty = ctx.createText(EMPTY_STYLE);
  empty.text = FEED_EMPTY_TEXT;
  container.addChild(empty);

  const textures = createEventTextures(ctx);
  const events = buildFeedEvents(scene);
  const rows = new Map<string, RowView>();
  let rowsOwnerTeamId: string | undefined;

  const destroyRow = (key: string) => { rows.get(key)?.container.destroy({ children: true }); rows.delete(key); };

  function createRow(event: FeedEvent, ownerTeamId: string): RowView {
    const { layout, height } = layoutFeedRow(event, ownerTeamId, measure);
    const row = new Container({ label: event.key });
    const addText = (value: string, textStyle: TextStyleOptions, x: number, y: number) => {
      const text = ctx.createText(textStyle);
      setTextIfChanged(text, value);
      text.position.set(x, y);
      row.addChild(text);
    };
    addText(clock(event.t), TIME_STYLE, 0, 0);
    const symbol = new Sprite(textures[event.kind]);
    symbol.tint = event.kind === 'chat' ? CHAT_ICON_COLOR : KILL_ICON_COLOR;
    symbol.position.set(TIME_WIDTH + COLUMN_GAP, (LINE_HEIGHT - ICON_WIDTH) / 2);
    row.addChild(symbol);
    for (const piece of layout.pieces) {
      const x = CONTENT_X + piece.x;
      if (piece.kind === 'text') {
        addText(piece.text, piece.style, x, piece.line * LINE_HEIGHT);
      } else if (piece.icon.startsWith('class:')) {
        const [, shipClass, color] = piece.icon.split(':');
        const sprite = ctx.classIconSprite(shipClass);
        sprite.tint = color;
        sprite.scale.set(CLASS_ICON_SIZE / Math.max(sprite.texture.width, sprite.texture.height));
        sprite.position.set(x + CLASS_ICON_SIZE / 2, piece.line * LINE_HEIGHT + LINE_HEIGHT / 2);
        row.addChild(sprite);
      }
    }
    stack.addChild(row);
    return { container: row, height };
  }

  return {
    container: root,
    update(frame: BroadcastFrameState) {
      if (rowsOwnerTeamId !== frame.ownerTeamId) {
        for (const key of [...rows.keys()]) destroyRow(key);
        rowsOwnerTeamId = frame.ownerTeamId;
      }
      const visible = visibleFeedEvents(events, frame.time);
      const keys = new Set(visible.map(e => e.key));
      for (const key of [...rows.keys()]) if (!keys.has(key)) destroyRow(key);
      const views = visible.map(event => {
        let view = rows.get(event.key);
        if (!view) { view = createRow(event, frame.ownerTeamId); rows.set(event.key, view); }
        return { event, view };
      });
      empty.visible = views.length === 0;
      // Bottom-anchored stack: the newest row ends at the window bottom, then shifts down while rows enter.
      let bottom = FEED_WINDOW_HEIGHT + feedStackOffset(views.map(({ event, view }) => ({ t: event.t, height: view.height })), frame.time, frame.reducedMotion);
      for (let i = views.length - 1; i >= 0; i--) {
        bottom -= views[i].view.height;
        views[i].view.container.y = bottom;
        bottom -= ROW_GAP;
      }
    },
    destroy() {
      rows.clear();
      root.destroy({ children: true });
      textures.chat.destroy(true);
      textures.kill.destroy(true);
    },
  };
}
