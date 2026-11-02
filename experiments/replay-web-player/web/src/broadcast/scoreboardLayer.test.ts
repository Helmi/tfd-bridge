import { describe, expect, it, vi } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { compareFleetShips } from '../engine/fleetOrder';
import { evaluateScene } from '../engine/timeline';
import type { EvaluatedShip, ReplayScene } from '../types';
import { COLORS } from './layout';
import type { BroadcastFrameState, StageContext } from './types';

vi.mock('pixi.js', () => {
  class Container {
    label: string;
    children: unknown[] = [];
    boundsArea?: { x: number; y: number; width: number; height: number };
    position = { x: 0, y: 0, set(x: number, y: number) { this.x = x; this.y = y; } };
    constructor(options: { label?: string } = {}) { this.label = options.label ?? ''; }
    addChild(child: unknown) { this.children.push(child); return child; }
    destroy() {}
  }
  class Graphics extends Container {
    roundRect() { return this; }
    fill() { return this; }
    circle() { return this; }
    stroke() { return this; }
    clear() { return this; }
  }
  class Rectangle { constructor(public x: number, public y: number, public width: number, public height: number) {} }
  return { Container, Graphics, Rectangle };
});

import { createScoreboardLayer, stripBounds, stripLayout } from './scoreboardLayer';

const owner = sampleScene.ships[0];
const scene: ReplayScene = { ...sampleScene, replay: { ...sampleScene.replay, perspectiveEntityId: owner.id } };

function fakeContext() {
  const texts: { text: string; assignments: number; style: { fill?: unknown } }[] = [];
  const sprites: { tint: unknown; alpha: number; scale: { set: ReturnType<typeof vi.fn> }; position: { set: ReturnType<typeof vi.fn> }; texture: { width: number; height: number }; destroy: ReturnType<typeof vi.fn> }[] = [];
  const ctx = {
    createText: (style: { fill?: unknown }) => {
      let value = '';
      const text = {
        assignments: 0, style: { ...style }, anchor: { set: vi.fn() }, position: { set: vi.fn() },
        get text() { return value; }, set text(next: string) { value = next; this.assignments++; },
      };
      texts.push(text);
      return text;
    },
    classIconSprite: () => {
      const sprite = { tint: 0xffffff as unknown, alpha: 1, scale: { set: vi.fn() }, position: { set: vi.fn() }, texture: { width: 72, height: 128 }, destroy: vi.fn() };
      sprites.push(sprite);
      return sprite;
    },
  } as unknown as StageContext;
  return { ctx, texts, sprites };
}

function frame(time: number): BroadcastFrameState {
  const state = evaluateScene(scene, time);
  const order = (a: EvaluatedShip, b: EvaluatedShip) => compareFleetShips(a.definition, b.definition);
  return {
    scene, time, state, selectedId: owner.id, ownerId: owner.id, ownerTeamId: owner.teamId,
    ally: state.ships.filter(s => s.definition.teamId === owner.teamId).sort(order),
    enemy: state.ships.filter(s => s.definition.teamId !== owner.teamId).sort(order),
    interactive: true, reducedMotion: false, selectShip() {},
  };
}

describe('scoreboard layer', () => {
  it('labels its container', () => {
    expect(createScoreboardLayer(fakeContext().ctx, scene).container.label).toBe('scoreboard');
  });

  it('creates the icons once and assigns text only when it changes', () => {
    const { ctx, texts, sprites } = fakeContext();
    const layer = createScoreboardLayer(ctx, scene);
    layer.update(frame(0));
    const created = sprites.length;
    expect(created).toBe(scene.ships.length);
    const assigned = texts.map(t => t.assignments);
    layer.update(frame(0));
    layer.update(frame(0.4));
    expect(sprites).toHaveLength(created);
    expect(texts.map(t => t.assignments)).toEqual(assigned);
    layer.update(frame(2));
    expect(texts.some(t => t.text === '00:02')).toBe(true);
    expect(texts.map(t => t.assignments).reduce((a, b) => a + b, 0)).toBe(assigned.reduce((a, b) => a + b, 0) + 1);
  });

  it('shows scores, map label and cap letters', () => {
    const { ctx, texts } = fakeContext();
    createScoreboardLayer(ctx, scene).update(frame(250));
    const values = texts.map(t => t.text);
    expect(values).toEqual(expect.arrayContaining(['518', '472', 'NORTHERN WATERS', '04:10', 'A', 'B', 'C']));
  });

  it('fits the icon into its 27x25 slot and dims sunk ships', () => {
    const { ctx, sprites } = fakeContext();
    const layer = createScoreboardLayer(ctx, scene);
    layer.update(frame(0));
    expect(sprites[0].scale.set).toHaveBeenCalledWith(Math.min(27 / 72, 25 / 128));
    expect(sprites.every(s => s.alpha === 1)).toBe(true);
    layer.update(frame(345));
    expect(sprites.filter(s => s.alpha === 0.45)).toHaveLength(4);
    expect(sprites.filter(s => s.alpha === 0.45).every(s => s.tint === '#68766d')).toBe(true);
  });

  it('gives every survivor icon a 25 x 27 box, 30 px apart, with the glyph centred in it', () => {
    const { ctx, sprites } = fakeContext();
    const layer = createScoreboardLayer(ctx, scene);
    const first = frame(0);
    layer.update(first);
    type Box = { label: string; boundsArea: { width: number; height: number }; position: { x: number; y: number }; children: { position: { set: ReturnType<typeof vi.fn> } }[] };
    const boxes = (layer.container as unknown as { children: Box[] }).children.filter(c => c.label === 'survivor');
    const friendly = first.ally.length;
    const left = 249 - (friendly * 30 - 3) / 2;
    expect(boxes).toHaveLength(scene.ships.length);
    boxes.slice(0, friendly).forEach((box, i) => {
      expect(box.boundsArea).toMatchObject({ width: 25, height: 27 });
      expect(box.position.x).toBeCloseTo(left + 1 + 30 * i);
      expect(box.position.y).toBe(93);
      expect(box.children[0].position.set).toHaveBeenCalledWith(12.5, 13.5);
    });
    expect(sprites).toHaveLength(boxes.length);
  });

  it('colours caps by owner', () => {
    const { ctx, texts } = fakeContext();
    const layer = createScoreboardLayer(ctx, scene);
    layer.update(frame(165));
    const caps = ['A', 'B', 'C'].map(letter => texts.find(t => t.text === letter)!.style.fill);
    expect(caps).toEqual([COLORS.green, COLORS.red, COLORS.red]);
  });
});

describe('survivor strip layout', () => {
  const friendly = stripBounds('friendly', 3);
  const enemy = stripBounds('enemy', 3);
  const SLOT = 27;
  const extent = (count: number, l: { pitch: number; scale: number }) => (count - 1) * l.pitch + SLOT * l.scale;

  it('bounds each side by its column edge and 8 px before the cap row', () => {
    // Three caps: 3 x 21 + 2 x 9 = 81 px wide, centred on x 540.
    expect(friendly).toEqual({ left: 38, right: 540 - 40.5 - 8 });
    expect(enemy).toEqual({ left: 540 + 40.5 + 8, right: 28 + 1024 - 10 });
  });

  it('keeps 12 ships exactly as before: 30 px pitch, full size, centred on the column', () => {
    expect(stripLayout(12, friendly, 249)).toEqual({ pitch: 30, scale: 1, x0: 249 - (12 * 30 - 3) / 2 });
    const right = stripLayout(12, enemy, 831);
    expect(right).toEqual({ pitch: 30, scale: 1, x0: 831 - 178.5 });
  });

  it('first closes the gap, then shrinks the icons', () => {
    const tight = stripLayout(16, friendly, 249);
    expect(tight.scale).toBe(1);
    expect(tight.pitch).toBeGreaterThanOrEqual(SLOT);
    expect(tight.pitch).toBeLessThan(30);
    const small = stripLayout(24, friendly, 249);
    expect(small.scale).toBeLessThan(1);
    expect(small.scale).toBeGreaterThanOrEqual(0.5);
    expect(small.pitch).toBeCloseTo(SLOT * small.scale);
  });

  it.each([12, 15, 16, 24, 30, 40, 60])('fits %i ships inside both bounds', count => {
    for (const bounds of [friendly, enemy]) {
      const layout = stripLayout(count, bounds, (bounds.left + bounds.right) / 2);
      expect(layout.scale).toBeGreaterThanOrEqual(0.5);
      expect(layout.x0).toBeGreaterThanOrEqual(bounds.left - 1e-9);
      expect(layout.x0 + extent(count, layout)).toBeLessThanOrEqual(bounds.right + 1e-9);
    }
  });

  it('keeps the friendly strip 8 px left of the cap row and the enemy strip 8 px right of it', () => {
    const f = stripLayout(24, friendly, 249);
    expect(f.x0 + extent(24, f)).toBeLessThanOrEqual(540 - 40.5 - 8 + 1e-9);
    const e = stripLayout(24, enemy, 831);
    expect(e.x0).toBeGreaterThanOrEqual(540 + 40.5 + 8 - 1e-9);
  });

  it('overlaps the icons at half size when even that does not fit', () => {
    const layout = stripLayout(40, friendly, 249);
    expect(layout.scale).toBe(0.5);
    expect(layout.pitch).toBeLessThan(SLOT * 0.5);
    expect(layout.x0 + extent(40, layout)).toBeCloseTo(friendly.right);
  });

  it('scales the drawn boxes of a crowded battle and keeps them inside the strip bounds', () => {
    const clones = Array.from({ length: 48 }, (_, i) => ({ ...sampleScene.ships[i % 2 ? 0 : sampleScene.ships.findIndex(s => s.teamId !== owner.teamId)], id: `bot-${i}`, teamId: i % 2 ? owner.teamId : 'enemy-team' }));
    const crowded: ReplayScene = { ...scene, ships: [owner, ...clones] as ReplayScene['ships'] };
    const { ctx, sprites } = fakeContext();
    const layer = createScoreboardLayer(ctx, crowded);
    const state = evaluateScene(crowded, 0);
    const split = (team: (s: EvaluatedShip) => boolean) => state.ships.filter(team);
    layer.update({
      scene: crowded, time: 0, state, selectedId: owner.id, ownerId: owner.id, ownerTeamId: owner.teamId,
      ally: split(s => s.definition.teamId === owner.teamId), enemy: split(s => s.definition.teamId !== owner.teamId),
      interactive: true, reducedMotion: false, selectShip() {},
    });
    type Box = { label: string; boundsArea: { width: number }; position: { x: number } };
    const boxes = (layer.container as unknown as { children: Box[] }).children.filter(c => c.label === 'survivor');
    expect(boxes).toHaveLength(crowded.ships.length);
    const sides = [friendly, enemy];
    const nAlly = state.ships.filter(s => s.definition.teamId === owner.teamId).length;
    const groups = [boxes.slice(0, nAlly), boxes.slice(nAlly)];
    groups.forEach((group, i) => {
      const bounds = sides[i];
      expect(group[0].position.x).toBeGreaterThanOrEqual(bounds.left - 1);
      const last = group[group.length - 1];
      expect(last.position.x + last.boundsArea.width).toBeLessThanOrEqual(bounds.right + 1e-9);
    });
    expect(sprites).toHaveLength(boxes.length);
    expect(groups[0][0].boundsArea.width).toBeLessThan(25);
  });
});
