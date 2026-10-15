import { describe, expect, it } from 'vitest';
import {
  backingSize, BOX, COLORS, FEED_HEIGHT, FEED_WINDOW_HEIGHT, FRAME, INTEL_COLUMN_WIDTH, MAP_COLUMN_WIDTH,
  PLAYER_HEIGHT, rendererResolution, ROSTER_ROW_HEIGHT, toFrame,
} from './layout';

// These values came from the last matching rules of the former broadcast.css (see git history).
describe('layout constants', () => {
  it('frame and grid columns', () => {
    expect(FRAME).toEqual({ width: 1920, height: 1080 });
    expect(`${MAP_COLUMN_WIDTH}px ${INTEL_COLUMN_WIDTH}px`).toBe('1080px 840px');
    expect(BOX.map.w).toBe(1080);
    expect(INTEL_COLUMN_WIDTH).toBe(840);
  });
  it('player, feed, feed window, ribbons and roster row sizes', () => {
    expect(PLAYER_HEIGHT).toBe(220);
    expect(FEED_HEIGHT).toBe(178);
    expect(FEED_WINDOW_HEIGHT).toBe(156);
    expect(ROSTER_ROW_HEIGHT).toBe(44);
    expect(BOX.ribbons.h).toBe(80);
  });
  it('colors and frame background', () => {
    expect(COLORS).toMatchObject({ muted: '#839b90', line: '#26392f', green: '#67d9a3', red: '#fa8274', gold: '#e6b855', frameBg: '#0b1711' });
  });
});

describe('region boxes', () => {
  it('stack the intel column down to the bottom margin', () => {
    expect(BOX.player.y + BOX.player.h).toBe(BOX.ribbons.y);
    expect(BOX.ribbons.y + BOX.ribbons.h).toBe(BOX.feed.y);
    expect(BOX.feed.y + BOX.feed.h).toBe(BOX.fleets.y);
    expect(BOX.fleets.y + BOX.fleets.h).toBe(1058);
    expect(BOX.intel.x + BOX.intel.w).toBe(1885);
  });
});

describe('toFrame', () => {
  it.each([0.5, 1, 1.5])('maps client pixels at scale %s with a letterbox offset', scale => {
    const rect = { left: 40, top: 12, width: 1920 * scale, height: 1080 * scale };
    expect(toFrame(rect, 40, 12)).toEqual({ x: 0, y: 0 });
    expect(toFrame(rect, 40 + 960 * scale, 12 + 540 * scale)).toEqual({ x: 960, y: 540 });
    expect(toFrame(rect, 40 + 1920 * scale, 12 + 1080 * scale)).toEqual({ x: 1920, y: 1080 });
  });
  it('returns coordinates outside the frame for clicks in the letterbox', () => {
    expect(toFrame({ left: 100, top: 0, width: 960, height: 540 }, 50, 0).x).toBeLessThan(0);
  });
});

describe('renderer resolution and backing size', () => {
  it('is viewport scale times dpr', () => {
    expect(rendererResolution(0.5, 2)).toBe(1);
    expect(rendererResolution(1.25, 1.5)).toBe(1.875);
  });
  it.each([1, 1.5, 2])('backing size at dpr %s', dpr => {
    const scale = 0.7;
    expect(backingSize(rendererResolution(scale, dpr))).toEqual({
      width: Math.round(1920 * scale * dpr),
      height: Math.round(1080 * scale * dpr),
    });
  });
  it('rounds like the renderer', () => {
    expect(backingSize(4 / 3)).toEqual({ width: 2560, height: 1440 });
  });
});
