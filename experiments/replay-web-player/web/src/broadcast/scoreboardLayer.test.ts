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

import { createScoreboardLayer } from './scoreboardLayer';

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
