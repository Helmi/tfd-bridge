import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { compareFleetShips } from '../engine/fleetOrder';
import { evaluateScene } from '../engine/timeline';
import type { EvaluatedShip, ReplayScene, ShipDefinition } from '../types';
import type { BroadcastFrameState, StageContext } from './types';

interface FakeNode {
  label?: string;
  children: FakeNode[];
  position: { x: number; y: number; set(x: number, y: number): void; copyFrom(p: { x: number; y: number }): void };
  alpha: number;
  visible: boolean;
  [key: string]: unknown;
}

vi.mock('pixi.js', () => {
  class Container {
    label?: string;
    children: unknown[] = [];
    alpha = 1;
    visible = true;
    hitArea: unknown;
    eventMode?: string;
    cursor?: string;
    accessible?: boolean;
    accessibleType?: string;
    accessibleTitle?: string;
    tabIndex?: number;
    listeners = new Map<string, Set<(...args: unknown[]) => void>>();
    position = { x: 0, y: 0, set(x: number, y: number) { this.x = x; this.y = y; }, copyFrom(p: { x: number; y: number }) { this.x = p.x; this.y = p.y; } };
    scale = { x: 1, y: 1, set(v: number) { this.x = v; this.y = v; } };
    constructor(options?: { label?: string }) { this.label = options?.label; }
    addChild(...nodes: unknown[]) { for (const node of nodes) { this.children = this.children.filter(child => child !== node); this.children.push(node); } }
    on(type: string, fn: (...args: never[]) => void) { (this.listeners.get(type) ?? this.listeners.set(type, new Set()).get(type)!).add(fn as never); }
    off(type: string, fn: (...args: never[]) => void) { this.listeners.get(type)?.delete(fn as never); }
    emit(type: string, ...args: unknown[]) { this.listeners.get(type)?.forEach(fn => fn(...args)); }
    destroy() {}
  }
  class Graphics extends Container {
    shapes: { kind: string; args: number[]; fill?: string; stroke?: { width: number; color: string } }[] = [];
    rect(...args: number[]) { this.shapes.push({ kind: 'rect', args }); return this; }
    fill(color: string) { this.shapes[this.shapes.length - 1].fill = color; return this; }
    stroke(style: { width: number; color: string }) { this.shapes[this.shapes.length - 1].stroke = style; return this; }
  }
  class Rectangle { constructor(public x: number, public y: number, public width: number, public height: number) {} }
  class TextStyle { constructor(public options: unknown) {} }
  const CanvasTextMetrics = { measureText: (value: string) => ({ width: Array.from(value).length * 8 }) };
  return { Container, Graphics, Rectangle, TextStyle, CanvasTextMetrics };
});

import { Container } from 'pixi.js';
import { createRosterLayer, rosterModels } from './rosterLayer';

const fakeText = () => {
  const node = new Container() as unknown as FakeNode & { text: string; style: { fill?: string }; anchor: { set: (x: number, y: number) => void }; width: number };
  node.text = '';
  node.style = {};
  node.anchor = { set() {} };
  Object.defineProperty(node, 'width', { get: () => Array.from(node.text).length * 8 });
  return node;
};
const fakeSprite = (shipClass: string) => {
  const node = new Container() as unknown as FakeNode & { shipClass: string; tint?: string; width: number; height: number };
  node.shipClass = shipClass;
  node.width = 72;
  node.height = 128;
  return node;
};
const ctx = {
  mode: 'interactive',
  createText: (style: { fill?: string }) => { const text = fakeText(); text.style = { fill: style.fill }; return text; },
  classIconSprite: fakeSprite,
  requestRender: vi.fn(),
} as unknown as StageContext;

const owner = sampleScene.ships[0];
const baseShips = (overrides: (ship: ShipDefinition, index: number) => Partial<ShipDefinition>): ShipDefinition[] =>
  sampleScene.ships.map((ship, index) => ({ ...ship, ...overrides(ship, index) }));

function frameOf(scene: ReplayScene, time: number, selectedId: string, interactive = true, selectShip: (id: string) => void = () => undefined): BroadcastFrameState {
  const state = evaluateScene(scene, time);
  const ownerShip = state.ships.find(s => s.definition.id === scene.replay.perspectiveEntityId)!;
  const order = (a: EvaluatedShip, b: EvaluatedShip) => compareFleetShips(a.definition, b.definition);
  return {
    scene, time, state, selectedId, ownerId: ownerShip.definition.id, ownerTeamId: ownerShip.definition.teamId,
    ally: state.ships.filter(s => s.definition.teamId === ownerShip.definition.teamId).sort(order),
    enemy: state.ships.filter(s => s.definition.teamId !== ownerShip.definition.teamId).sort(order),
    interactive, reducedMotion: false, selectShip,
  };
}

const scene: ReplayScene = {
  ...sampleScene,
  replay: { ...sampleScene.replay, perspectiveEntityId: owner.id },
  ships: baseShips(ship => ship.id === owner.id ? { relation: 'self' as const, divisionId: '7', divisionLabel: 'A' } : {}),
};

const rows = (layer: { container: unknown }) => {
  const [, ally, enemy] = (layer.container as { children: { children: FakeNode[] }[] }).children;
  return { ally: ally.children, enemy: enemy.children };
};
const textsOf = (row: FakeNode) => row.children.filter(child => 'text' in child) as unknown as { text: string; style: { fill?: string } }[];

describe('roster row view-model', () => {
  const shuffled: Array<Pick<ShipDefinition, 'shipName' | 'shipClass' | 'tier'>> = [
    { shipName: 'Zulu DD', shipClass: 'destroyer', tier: 10 },
    { shipName: 'Alpha BB', shipClass: 'battleship', tier: 8 },
    { shipName: 'Submarine', shipClass: 'submarine', tier: 10 },
    { shipName: 'Unknown BB', shipClass: 'battleship' },
    { shipName: 'Zulu BB', shipClass: 'battleship', tier: 10 },
    { shipName: 'Cruiser', shipClass: 'cruiser', tier: 9 },
    { shipName: 'Carrier', shipClass: 'carrier', tier: 8 },
    { shipName: 'Bravo BB', shipClass: 'battleship', tier: 10 },
    { shipName: 'Super BB', shipClass: 'battleship', tier: 11 },
  ];
  const ordered: ReplayScene = {
    ...scene,
    ships: ['allies', 'enemies'].flatMap(teamId => shuffled.map((ship, i) => ({
      ...owner, ...ship, teamId, id: `${teamId}-${i}`, relation: undefined, divisionId: undefined,
      divisionLabel: undefined, playerName: `Player ${i}`, health: [{ t: 0, value: 100 }, { t: 20, value: 0 }],
    }))),
    replay: { ...scene.replay, perspectiveEntityId: 'allies-0' },
  };
  const expected = ['Carrier', 'Super BB', 'Bravo BB', 'Zulu BB', 'Alpha BB', 'Unknown BB', 'Cruiser', 'Zulu DD', 'Submarine'];

  it('keeps both rosters in class, tier and name order through selection and sinking', () => {
    for (const time of [0, 30]) for (const selectedId of ['allies-0', 'enemies-4']) {
      const models = rosterModels(frameOf(ordered, time, selectedId));
      for (const side of [models.ally, models.enemy]) {
        expect(side.map(row => row.shipName)).toEqual(expected);
        expect(side.every(row => row.sunk)).toBe(time === 30);
        expect(side.filter(row => row.self).map(row => row.id)).toEqual(selectedId.startsWith('allies') ? (side === models.ally ? [selectedId] : []) : (side === models.enemy ? [selectedId] : []));
      }
    }
  });

  it('describes a row for screen readers and colours the division and icon', () => {
    const own = rosterModels(frameOf(scene, 30, 'a-02')).ally;
    const clanless = own.find(row => row.id === 'a-04')!;
    const clan = own.find(row => row.id === 'a-01')!;
    expect(clan.playerLine).toBe('[TFD] Helmi');
    expect(clanless.playerLine).toBe('Kestrel');
    expect(clan.divisionColor).toBe('#ffd369');
    expect(clan.iconColor).toBe('#ffd369');
    expect(own.find(row => row.id === 'a-02')!.iconColor).toBe('#ff69b4');
    expect(clanless.iconColor).toBe('#67d9a3');
    expect(clan.title).toMatch(/^Des Moines, Helmi, [\d,]+ \/ 50,600 HP$/);
    expect(rosterModels(frameOf(scene, 30, 'a-02')).enemy[0].teamColor).toBe('#fa8274');
  });
});

describe('roster layer', () => {
  let selectShip: Mock<(id: string) => void>;
  beforeEach(() => { selectShip = vi.fn<(id: string) => void>(); });

  it('draws both columns with rows 44 px apart and the divider lines', () => {
    const layer = createRosterLayer(ctx, scene);
    layer.update(frameOf(scene, 0, owner.id, false, selectShip));
    expect((layer.container as unknown as FakeNode).label).toBe('rosters');
    const { ally, enemy } = rows(layer);
    expect(ally).toHaveLength(12);
    expect(enemy).toHaveLength(12);
    expect(ally.map(row => row.position.y)).toEqual(Array.from({ length: 12 }, (_, i) => 527 + i * 44));
    expect(ally[0].position.x).toBe(1115);
    expect(enemy[0].position.x).toBe(1539);
    const lines = (layer.container as unknown as { children: { shapes: { args: number[] }[] }[] }).children[0].shapes.map(s => s.args);
    expect(lines).toEqual([[1115, 512, 770, 1], [1514, 527, 1, 531]]);
  });

  it('makes rows hit containers with accessibility only when interactive', () => {
    const layer = createRosterLayer(ctx, scene);
    layer.update(frameOf(scene, 0, owner.id, true, selectShip));
    const { ally, enemy } = rows(layer);
    for (const row of [...ally, ...enemy] as unknown as FakeNode[]) {
      expect(row.eventMode).toBe('static');
      expect(row.cursor).toBe('pointer');
      expect(row.accessible).toBe(true);
      expect(row.accessibleType).toBe('button');
      expect(row.accessibleTitle).toMatch(/, [\d,]+ \/ [\d,]+ HP$/);
    }
    const first = ally[0] as unknown as Container & { label: string };
    (first as unknown as { emit(t: string): void }).emit('pointertap');
    (first as unknown as { emit(t: string): void }).emit('click');
    expect(selectShip).toHaveBeenCalledTimes(2);
    expect(selectShip).toHaveBeenNthCalledWith(1, first.label.replace('roster-row:', ''));
    expect(selectShip).toHaveBeenNthCalledWith(2, first.label.replace('roster-row:', ''));
  });

  it('creates no hit containers and no accessibility props in export', () => {
    const layer = createRosterLayer(ctx, scene);
    layer.update(frameOf(scene, 0, owner.id, false, selectShip));
    const { ally, enemy } = rows(layer);
    for (const row of [...ally, ...enemy] as unknown as FakeNode[]) {
      expect(row.eventMode).toBeUndefined();
      expect(row.cursor).toBeUndefined();
      expect(row.accessible).toBeUndefined();
      expect(row.accessibleType).toBeUndefined();
      expect(row.accessibleTitle).toBeUndefined();
      expect((row as unknown as { listeners: Map<string, unknown> }).listeners.size).toBe(0);
    }
  });

  it('removes the hit behaviour when the frame turns non-interactive', () => {
    const layer = createRosterLayer(ctx, scene);
    layer.update(frameOf(scene, 0, owner.id, true, selectShip));
    layer.update(frameOf(scene, 0, owner.id, false, selectShip));
    const row = rows(layer).ally[0] as unknown as FakeNode & { emit(t: string): void };
    expect(row.accessible).toBe(false);
    expect(row.eventMode).toBe('passive');
    row.emit('click');
    expect(selectShip).not.toHaveBeenCalled();
  });

  it('draws a 2 px gold ring while the accessibility overlay focuses a row, and hides it on blur', () => {
    vi.mocked(ctx.requestRender).mockClear();
    const layer = createRosterLayer(ctx, scene);
    layer.update(frameOf(scene, 0, owner.id, true));
    const [first, second] = rows(layer).ally as unknown as (FakeNode & { emit(t: string, ...a: unknown[]): void })[];
    const ring = (row: FakeNode) => row.children.at(-1) as unknown as FakeNode & { shapes: { kind: string; args: number[]; stroke?: { width: number; color: string } }[] };
    expect(ring(first).visible).toBe(false);
    expect(ring(first).shapes[0]).toMatchObject({ kind: 'rect', args: [-1, -1, 371 + 2, 44 + 2], stroke: { width: 2, color: '#e6b855' } });
    // Pixi's overlay: focus is a `mouseover` without a native event, blur a `mouseout`.
    first.emit('mouseover', {});
    expect(ring(first).visible).toBe(true);
    // The stage does not tick while paused: each focus change asks for a redraw.
    expect(ctx.requestRender).toHaveBeenCalledTimes(1);
    // Tab to the next row: the old ring goes, the new one shows.
    first.emit('mouseout');
    second.emit('mouseover', {});
    expect(ring(first).visible).toBe(false);
    expect(ring(second).visible).toBe(true);
    second.emit('mouseout');
    expect(ring(second).visible).toBe(false);
    expect(ctx.requestRender).toHaveBeenCalledTimes(4);
    // A real mouse hover has a native event and draws nothing.
    first.emit('mouseover', { nativeEvent: {} });
    expect(ring(first).visible).toBe(false);
  });

  it('drops the ring on real mouse movement, as Pixi drops its overlay', () => {
    const listeners = new Map<string, (event: unknown) => void>();
    vi.stubGlobal('document', {
      addEventListener: (type: string, fn: (event: unknown) => void) => listeners.set(type, fn),
      removeEventListener: (type: string) => listeners.delete(type),
    });
    try {
      const layer = createRosterLayer(ctx, scene);
      layer.update(frameOf(scene, 0, owner.id, true));
      const first = rows(layer).ally[0] as unknown as FakeNode & { emit(t: string, ...a: unknown[]): void };
      first.emit('mouseover', {});
      expect((first.children.at(-1) as FakeNode).visible).toBe(true);
      listeners.get('mousemove')!({ movementX: 0, movementY: 0 });
      expect((first.children.at(-1) as FakeNode).visible).toBe(true);
      listeners.get('mousemove')!({ movementX: 3, movementY: 0 });
      expect((first.children.at(-1) as FakeNode).visible).toBe(false);
      expect(listeners.has('mousemove')).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('shows sunk, self, division and long names in the Pixi state', () => {
    const long = 'Bismarck\u00a0\'41 with a very long official name';
    const longScene: ReplayScene = {
      ...scene,
      ships: baseShips(ship => ({
        ...(ship.id === 'a-02' ? { shipName: long, playerName: 'A very long player name that cannot fit', clan: 'LONGCLAN' } : {}),
        ...(ship.id === 'a-01' ? { health: [{ t: 0, value: ship.maxHealth }, { t: 20, value: 0 }] } : {}),
        ...(ship.id === 'a-03' ? { relation: 'ally' as const, divisionId: '7', divisionLabel: 'A' } : {}),
        ...(ship.id === 'a-04' ? { divisionId: '9', divisionLabel: 'B' } : {}),
        ...(ship.id === 'a-01' ? { relation: 'self' as const, divisionId: '7', divisionLabel: 'A' } : {}),
      })),
      replay: { ...scene.replay, perspectiveEntityId: 'a-01' },
    };
    const layer = createRosterLayer(ctx, longScene);
    layer.update(frameOf(longScene, 0, 'a-01', true, selectShip));
    const byLabel = (id: string) => rows(layer).ally.find(row => row.label === `roster-row:${id}`)! as unknown as FakeNode;
    const alive = byLabel('a-01');
    expect(alive.alpha).toBe(1);
    // The self marker is the first child.
    expect((alive.children[0] as FakeNode).visible).toBe(true);
    expect(textsOf(alive).map(t => t.style.fill)).toEqual(['#ffd369', '#ff69b4', '#b8c2bc']);

    layer.update(frameOf(longScene, 30, 'a-03', true, selectShip));
    expect(byLabel('a-01').alpha).toBe(0.42);
    expect((byLabel('a-01').children[0] as FakeNode).visible).toBe(false);
    expect((byLabel('a-03').children[0] as FakeNode).visible).toBe(true);
    const sunk = byLabel('a-01');
    expect((sunk.children[1] as unknown as { tint: string; alpha: number }).tint).toBe('#68766d');
    expect((sunk.children[1] as unknown as { alpha: number }).alpha).toBe(0.45);
    expect(textsOf(sunk).map(t => t.style.fill)).toEqual(['#ffd369', '#e1e9e2', '#92aa9b']);
    // division B is not the own division: team colour
    expect(textsOf(byLabel('a-04'))[0].style.fill).toBe('#67d9a3');

    const longName = textsOf(byLabel('a-02'));
    expect(longName.map(t => t.text)).toEqual([expect.stringMatching(/^Bismarck.*…$/), expect.stringMatching(/^\[LONGCLAN\] A very.*…$/)]);
    // Fits: 271 px column text width, 8 px per character in the fake measure.
    expect(Array.from(longName[0].text).length * 8).toBeLessThanOrEqual(271);
    expect(Array.from(longName[1].text).length * 8).toBeLessThanOrEqual(271);
  });

  it('scales the health fill by health / maxHealth and hides it when empty', () => {
    const layer = createRosterLayer(ctx, scene);
    layer.update(frameOf(scene, 0, owner.id, false, selectShip));
    const row = rows(layer).ally[0] as unknown as FakeNode;
    const fill = row.children[row.children.length - 1] as unknown as { scale: { x: number }; visible: boolean };
    expect(fill.scale.x).toBeCloseTo(1);
    layer.update(frameOf(scene, 100000, owner.id, false, selectShip));
    expect(fill.scale.x).toBeGreaterThanOrEqual(0);
    expect(fill.scale.x).toBeLessThanOrEqual(1);
  });
});
