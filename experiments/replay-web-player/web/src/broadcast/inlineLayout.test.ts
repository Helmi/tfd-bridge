import { describe, expect, it } from 'vitest';
import { layoutInline, type InlineToken } from './inlineLayout';
import type { MeasureText } from './text';

// Every character is 10 px wide.
const measure: MeasureText = value => Array.from(value).length * 10;
const a = { fontSize: 15 };
const b = { fontSize: 14 };
const text = (value: string, style = a): InlineToken => ({ text: value, style });
const lines = (tokens: InlineToken[], width: number) => {
  const { pieces, lineCount } = layoutInline(tokens, width, measure);
  return { lineCount, rows: Array.from({ length: lineCount }, (_, i) => pieces.filter(p => p.line === i)) };
};

describe('layoutInline', () => {
  it('keeps runs that fit on one line, side by side', () => {
    const { lineCount, pieces } = layoutInline([text('Ship', a), text(' | Name', b)], 200, measure);
    expect(lineCount).toBe(1);
    expect(pieces).toEqual([
      { kind: 'text', text: 'Ship', style: a, x: 0, line: 0 },
      { kind: 'text', text: ' | Name', style: b, x: 40, line: 0 },
    ]);
  });

  it('merges neighbouring words of the same style into one piece', () => {
    const { pieces } = layoutInline([text('one two three')], 200, measure);
    expect(pieces).toEqual([{ kind: 'text', text: 'one two three', style: a, x: 0, line: 0 }]);
  });

  it('wraps at a space and drops the space at the break', () => {
    const { lineCount, rows } = lines([text('aaaa bbbb cccc')], 90);
    expect(lineCount).toBe(2);
    expect(rows[0]).toEqual([{ kind: 'text', text: 'aaaa bbbb', style: a, x: 0, line: 0 }]);
    expect(rows[1]).toEqual([{ kind: 'text', text: 'cccc', style: a, x: 0, line: 1 }]);
  });

  it('fills a line up to the exact width', () => {
    expect(lines([text('aaaa bbbb')], 90).lineCount).toBe(1);
    expect(lines([text('aaaa bbbb')], 89).lineCount).toBe(2);
  });

  it('collapses runs of spaces and drops spaces at the start and end', () => {
    const { pieces } = layoutInline([text('  a   b  ')], 200, measure);
    expect(pieces).toEqual([{ kind: 'text', text: 'a b', style: a, x: 0, line: 0 }]);
  });

  it('moves a long word to its own line, then breaks it by characters', () => {
    const { lineCount, rows } = lines([text('ab '), text('abcdefghijkl')], 50);
    expect(lineCount).toBe(4);
    expect(rows.map(r => r.map(p => p.kind === 'text' ? p.text : '').join(''))).toEqual(['ab', 'abcde', 'fghij', 'kl']);
  });

  it('breaks a long word without a preceding line when it starts the text', () => {
    const { rows } = lines([text('abcdefg')], 40);
    expect(rows.map(r => r.map(p => p.kind === 'text' ? p.text : '').join(''))).toEqual(['abcd', 'efg']);
  });

  it('puts an icon box at the start of a line', () => {
    const { pieces } = layoutInline([{ icon: 'ship', width: 19 }, text('Name')], 200, measure);
    expect(pieces).toEqual([
      { kind: 'icon', icon: 'ship', width: 19, x: 0, line: 0 },
      { kind: 'text', text: 'Name', style: a, x: 19, line: 0 },
    ]);
  });

  it('glues an icon to the word after it and wraps both together', () => {
    const { rows } = lines([text('aaaa '), { icon: 'ship', width: 20 }, text('bbbb')], 80);
    expect(rows[0]).toEqual([{ kind: 'text', text: 'aaaa', style: a, x: 0, line: 0 }]);
    expect(rows[1]).toEqual([
      { kind: 'icon', icon: 'ship', width: 20, x: 0, line: 1 },
      { kind: 'text', text: 'bbbb', style: a, x: 20, line: 1 },
    ]);
  });

  it('places an icon in the middle of a line', () => {
    const { pieces } = layoutInline([text('ab '), { icon: 'ship', width: 20 }, text('cd')], 200, measure);
    expect(pieces.map(p => p.x)).toEqual([0, 30, 50]);
    expect(pieces[1]).toMatchObject({ kind: 'icon', x: 30 });
  });

  it('does not break at a non-breaking space', () => {
    const { rows } = lines([text('aa\u00a0bb cc')], 50);
    expect(rows.map(r => r.map(p => p.kind === 'text' ? p.text : '').join(''))).toEqual(['aa\u00a0bb', 'cc']);
  });

  it('returns one empty line for no tokens', () => {
    expect(layoutInline([], 100, measure)).toEqual({ pieces: [], lineCount: 1 });
  });
});
