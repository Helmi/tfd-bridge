import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  backingSize, BOX, COLORS, FEED_HEIGHT, FEED_WINDOW_HEIGHT, INTEL_COLUMN_WIDTH, MAP_COLUMN_WIDTH,
  PLAYER_HEIGHT, rendererResolution, ROSTER_ROW_HEIGHT, toFrame,
} from './layout';

const css = readFileSync(new URL('../components/broadcast.css', import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

/** Value of the LAST top-level declaration of one of `props` in a rule whose selector list has `selector`. */
function lastDeclaration(selector: string, props: string[]): { prop: string; value: string } | undefined {
  let found: { prop: string; value: string } | undefined;
  let index = 0;
  while (index < css.length) {
    const open = css.indexOf('{', index);
    if (open < 0) break;
    const prelude = css.slice(index, open).trim();
    let depth = 1;
    let close = open + 1;
    for (; close < css.length && depth > 0; close++) depth += css[close] === '{' ? 1 : css[close] === '}' ? -1 : 0;
    index = close;
    if (prelude.startsWith('@')) continue; // conditional rules never set these values
    if (!prelude.split(',').some(part => part.trim() === selector)) continue;
    for (const declaration of css.slice(open + 1, close - 1).split(';')) {
      const colon = declaration.indexOf(':');
      const prop = declaration.slice(0, colon).trim();
      if (colon > 0 && props.includes(prop)) found = { prop, value: declaration.slice(colon + 1).trim() };
    }
  }
  return found;
}

const px = (value: string | undefined) => Number.parseFloat(value ?? 'NaN');
/** `flex-basis` of a `flex` shorthand ("0 0 80px") or the longhand. */
const flexBasis = (selector: string) => {
  const last = lastDeclaration(selector, ['flex', 'flex-basis']);
  return px(last?.prop === 'flex' ? last.value.split(/\s+/)[2] : last?.value);
};

describe('layout constants match the last matching rule in broadcast.css', () => {
  it('grid columns', () => {
    expect(lastDeclaration('.broadcast', ['grid-template-columns'])?.value).toBe(`${MAP_COLUMN_WIDTH}px ${INTEL_COLUMN_WIDTH}px`);
    expect(BOX.map.w).toBe(1080);
    expect(INTEL_COLUMN_WIDTH).toBe(840);
  });
  it('player, feed, feed window and roster row sizes', () => {
    expect(flexBasis('.broadcast .player')).toBe(PLAYER_HEIGHT);
    expect(flexBasis('.broadcast .feed')).toBe(FEED_HEIGHT);
    expect(px(lastDeclaration('.broadcast .feed-window', ['height'])?.value)).toBe(FEED_WINDOW_HEIGHT);
    expect(px(lastDeclaration('.broadcast .roster-row', ['height'])?.value)).toBe(ROSTER_ROW_HEIGHT);
    expect(flexBasis('.broadcast .ribbons')).toBe(BOX.ribbons.h);
  });
  it.each(['muted', 'line', 'green', 'red', 'gold'] as const)('color --%s', name => {
    expect(lastDeclaration('.broadcast', [`--${name}`])?.value.toLowerCase()).toBe(COLORS[name]);
  });
  it('frame background', () => {
    expect(lastDeclaration('.broadcast', ['background'])?.value.toLowerCase()).toBe(COLORS.frameBg);
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
