import { describe, expect, it, vi } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import type { ReplayScene } from '../types';
import type { MeasureText } from './text';
import type { BroadcastFrameState, StageContext } from './types';

const fake = vi.hoisted(() => {
  class Node {
    label = '';
    children: Node[] = [];
    parent?: Node;
    destroyed = false;
    visible = true;
    mask?: Node;
    y = 0;
    position = { x: 0, y: 0, set: (x: number, y: number) => { this.position.x = x; this.position.y = y; if (this.constructor.name !== 'Container') this.y = y; } };
    scale = { set: vi.fn() };
    constructor(options?: { label?: string }) { this.label = options?.label ?? ''; }
    addChild(child: Node) { child.parent = this; this.children.push(child); return child; }
    destroy() { this.destroyed = true; this.parent?.children.splice(this.parent.children.indexOf(this), 1); [...this.children].forEach(child => child.destroy()); }
  }
  class Graphics extends Node {
    rects: number[][] = [];
    rect(...args: number[]) { this.rects.push(args); return this; }
    fill() { return this; }
    svg() { return this; }
  }
  class Sprite extends Node { tint: unknown; texture: unknown; constructor(texture?: unknown) { super(); this.texture = texture; } }
  return { Node, Graphics, Sprite, svgCalls: 0 };
});

vi.mock('pixi.js', () => ({
  Container: class Container extends fake.Node {},
  Graphics: fake.Graphics,
  Sprite: fake.Sprite,
  Rectangle: class { constructor(public x: number, public y: number, public width: number, public height: number) {} },
  CanvasTextMetrics: { measureText: () => ({ width: 0 }) },
  TextStyle: class {},
}));

import { createFeedLayer, FEED_EMPTY_TEXT, feedRowHeight, feedRowTokens, feedStackOffset, layoutFeedRow, visibleFeedEvents } from './feedLayer';
import { buildFeedEvents, ENTRANCE_SECONDS, feedEntranceRemaining, ROW_GAP, VIEWPORT_ROWS } from '../components/BattleFeed';

// Every character is 8 px wide: 697 px holds 87 characters.
const measure: MeasureText = value => Array.from(value).length * 8;

const owner = { ...sampleScene.ships[0], id: 'owner', teamId: 'a', shipName: 'Owner ship', playerName: 'Owner', clan: 'HOME' };
const enemy = { ...sampleScene.ships[1], id: 'enemy', teamId: 'b', shipName: 'Enemy ship', playerName: 'Opponent', clan: 'AWAY' };
const base: ReplayScene = { ...sampleScene, ships: [owner, enemy], chat: [], kills: [] };

describe('feed model', () => {
  const scene: ReplayScene = {
    ...base,
    kills: [{ t: 30, killerId: owner.id, victimId: enemy.id }],
    chat: [
      { id: 'later', t: 20, senderName: 'Later', channel: 'team', message: 'Second' },
      { id: 'earlier', t: 10, senderName: 'Earlier', channel: 'team', message: 'First' },
    ],
  };
  const events = buildFeedEvents(scene);

  it('shows arrived events oldest first and hides future events', () => {
    expect(visibleFeedEvents(events, 25).map(e => e.key)).toEqual(['chat-1', 'chat-0']);
    expect(visibleFeedEvents(events, 100).map(e => e.key)).toEqual(['chat-1', 'chat-0', 'kill-0']);
  });

  it('is empty before the first event, also after seeking back', () => {
    expect(visibleFeedEvents(events, 100)).toHaveLength(3);
    expect(visibleFeedEvents(events, 0)).toEqual([]);
  });

  it('keeps the newest rows plus the entering ones', () => {
    const many = buildFeedEvents({ ...base, chat: Array.from({ length: 20 }, (_, i) => ({ id: `${i}`, t: i, senderName: 'X', channel: 'team', message: 'm' })) });
    expect(visibleFeedEvents(many, 100)).toHaveLength(VIEWPORT_ROWS);
    expect(visibleFeedEvents(many, 100).at(-1)?.key).toBe('chat-19');
    // Time 20.5: events 19 and 18 entered less than 3.2 s ago.
    expect(visibleFeedEvents(many, 20.5)).toHaveLength(VIEWPORT_ROWS + 2);
  });

  it('resolves a chat sender by id first, then by unique name', () => {
    const named: ReplayScene = { ...base, chat: [
      { id: '1', t: 1, senderId: enemy.id, senderName: owner.playerName, channel: 'global', message: 'ID wins' },
      { id: '2', t: 2, senderName: owner.playerName, channel: 'team', message: 'Legacy name' },
      { id: '3', t: 3, senderName: 'Absent', channel: 'system', message: 'No ship' },
    ] };
    const text = (index: number) => feedRowTokens(buildFeedEvents(named)[index], 'a').flatMap(t => 'text' in t ? [t.text] : []).join('|');
    expect(text(0)).toContain('Absent');
    expect(text(1)).toContain('Owner\u00a0ship');
    expect(text(1)).toContain('| [HOME] Owner');
    expect(text(2)).toContain('Enemy\u00a0ship');
    expect(text(2)).toContain('| [AWAY] Opponent');
  });

  it('computes row height as max(24, lines x 22)', () => {
    expect(feedRowHeight(1)).toBe(24);
    expect(feedRowHeight(2)).toBe(44);
    expect(feedRowHeight(3)).toBe(66);
  });

  it('wraps a long chat message to more lines and grows the row', () => {
    const event = buildFeedEvents({ ...base, chat: [{ id: '1', t: 1, senderId: owner.id, senderName: 'Owner', channel: 'team', message: 'word '.repeat(40).trim() }] })[0];
    expect(layoutFeedRow(event, 'a', measure).layout.lineCount).toBeGreaterThan(1);
    const short = buildFeedEvents({ ...base, chat: [{ id: '1', t: 1, senderId: owner.id, senderName: 'Owner', channel: 'team', message: 'gg' }] })[0];
    expect(layoutFeedRow(short, 'a', measure)).toMatchObject({ height: 24, layout: { lineCount: 1 } });
  });

  it('offsets the stack by the entrance remainder, and not at all with reduced motion', () => {
    const rows = [{ t: 10, height: 24 }, { t: 12, height: 44 }];
    const expected = 26 * feedEntranceRemaining(3) + 46 * feedEntranceRemaining(1);
    expect(feedStackOffset(rows, 13, false)).toBeCloseTo(expected);
    expect(feedStackOffset(rows, 12 + ENTRANCE_SECONDS + 1, false)).toBe(0);
    expect(feedStackOffset(rows, 12, true)).toBe(0);
  });
});

describe('feed layer', () => {
  const textObjects: { text: string; style: { fontSize?: number }; destroyed: boolean; visible: boolean; position: { x: number; y: number; set(x: number, y: number): void }; parent?: unknown }[] = [];
  const makeCtx = (): StageContext => ({
    mode: 'export',
    renderer: { generateTexture: vi.fn(() => ({ destroy: vi.fn() })) } as never,
    resolution: 1,
    mapSupersampling: false,
    toFrame: () => ({ x: 0, y: 0 }),
    createText: style => {
      const node = new fake.Node() as unknown as (typeof textObjects)[number] & { destroyed: boolean };
      Object.assign(node, { text: '', style });
      textObjects.push(node);
      return node as never;
    },
    classIconSprite: () => Object.assign(new fake.Sprite({ width: 32, height: 32 }), { rotation: 0, anchor: { set() {} } }) as never,
    requestRender: () => {},
  });
  const frame = (scene: ReplayScene, time: number, reducedMotion = false): BroadcastFrameState => ({
    scene, time, ownerTeamId: 'a', reducedMotion, interactive: false,
  }) as BroadcastFrameState;
  const feedScene: ReplayScene = {
    ...base,
    kills: [{ t: 50, killerId: owner.id, victimId: enemy.id }],
    chat: [{ id: '1', t: 10, senderId: enemy.id, senderName: 'Opponent', channel: 'team', message: 'hello' }],
  };
  const windowOf = (layer: ReturnType<typeof createFeedLayer>) => layer.container.children[1] as unknown as InstanceType<typeof fake.Node> & { mask: InstanceType<typeof fake.Graphics>; position: { x: number; y: number } };
  const rowsOf = (layer: ReturnType<typeof createFeedLayer>) => (windowOf(layer).children[1] as InstanceType<typeof fake.Node>).children;

  it('is labelled, masked at 156 px, and shows the empty text with no events', () => {
    textObjects.length = 0;
    const layer = createFeedLayer(makeCtx(), base, measure);
    layer.update(frame(base, 5));
    expect(layer.container.label).toBe('feed');
    expect(windowOf(layer).mask.rects).toEqual([[0, 0, 770, 156]]);
    expect(windowOf(layer).position).toMatchObject({ x: 1115, y: 345 });
    const empty = textObjects.find(t => t.text === FEED_EMPTY_TEXT)!;
    expect(empty.visible).toBe(true);
    expect(rowsOf(layer)).toHaveLength(0);
  });

  it('draws the 1 px separator at the top of the feed box, outside the mask', () => {
    const layer = createFeedLayer(makeCtx(), base, measure);
    const line = layer.container.children[0] as unknown as InstanceType<typeof fake.Graphics>;
    expect(line.rects).toEqual([[1115, 334, 771, 1]]);
    expect(windowOf(layer).children).not.toContain(line);
  });

  it('pools rows by key: the same Text survives updates, and is destroyed when the row leaves', () => {
    textObjects.length = 0;
    const layer = createFeedLayer(makeCtx(), feedScene, measure);
    layer.update(frame(feedScene, 20));
    const row = rowsOf(layer)[0];
    const created = textObjects.length;
    const time = textObjects.find(t => t.text === '00:10')!;
    layer.update(frame(feedScene, 21));
    layer.update(frame(feedScene, 22));
    expect(textObjects).toHaveLength(created);
    expect(rowsOf(layer)[0]).toBe(row);
    expect(time.destroyed).toBe(false);
    expect(textObjects.find(t => t.text === FEED_EMPTY_TEXT)!.visible).toBe(false);
    layer.update(frame(feedScene, 5));
    expect(rowsOf(layer)).toHaveLength(0);
    expect(time.destroyed).toBe(true);
    expect(textObjects.find(t => t.text === FEED_EMPTY_TEXT)!.visible).toBe(true);
    // Seeking forward again rebuilds the row.
    layer.update(frame(feedScene, 60));
    expect(rowsOf(layer).map(r => r.label)).toEqual(['chat-0', 'kill-0']);
  });

  it('stacks rows from the window bottom, and shifts them while entering', () => {
    textObjects.length = 0;
    const layer = createFeedLayer(makeCtx(), feedScene, measure);
    layer.update(frame(feedScene, 100));
    const [chat, kill] = rowsOf(layer);
    expect(kill.y).toBe(156 - 24);
    expect(chat.y).toBe(156 - 24 - ROW_GAP - 24);
    layer.update(frame(feedScene, 50));
    expect(kill.y).toBeCloseTo(156 - 24 + 26);
    layer.update(frame(feedScene, 50, true));
    expect(kill.y).toBe(156 - 24);
  });

  it('destroys everything on destroy', () => {
    const layer = createFeedLayer(makeCtx(), feedScene, measure);
    layer.update(frame(feedScene, 100));
    const window = windowOf(layer);
    layer.destroy();
    expect((layer.container as unknown as { destroyed: boolean }).destroyed).toBe(true);
    expect((window as unknown as { destroyed: boolean }).destroyed).toBe(true);
  });
});
