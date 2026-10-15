import type { TextStyleOptions } from 'pixi.js';
import type { MeasureText } from './text';

export type InlineToken =
  | { text: string; style: TextStyleOptions }
  | { icon: string; width: number };

export type InlinePiece =
  | { kind: 'text'; text: string; style: TextStyleOptions; x: number; line: number }
  | { kind: 'icon'; icon: string; width: number; x: number; line: number };

export interface InlineLayout { pieces: InlinePiece[]; lineCount: number }

type Atom =
  | { kind: 'text'; text: string; style: TextStyleOptions; width: number }
  | { kind: 'icon'; icon: string; width: number };

const isSpace = (char: string) => char === ' ' || char === '\t' || char === '\n';

/**
 * Greedy line breaker for mixed inline runs, after CSS `white-space: normal` with
 * `overflow-wrap: anywhere`. Words break at ordinary spaces (runs of spaces collapse to one,
 * spaces at a line start or end are dropped). Non-breaking spaces and icon boxes glue to their
 * word. A word wider than `maxWidth` moves to its own line first, then breaks by characters.
 */
export function layoutInline(tokens: InlineToken[], maxWidth: number, measure: MeasureText): InlineLayout {
  const pieces: InlinePiece[] = [];
  let line = 0;
  let x = 0;
  let pendingSpace: { style: TextStyleOptions; width: number } | undefined;
  let word: Atom[] = [];
  let wordWidth = 0;
  let lastWasSpace = false;

  const place = (atom: Atom) => {
    const previous = pieces[pieces.length - 1];
    if (atom.kind === 'text' && previous?.kind === 'text' && previous.line === line && previous.style === atom.style) {
      previous.text += atom.text;
    } else if (atom.kind === 'text') {
      pieces.push({ kind: 'text', text: atom.text, style: atom.style, x, line });
    } else {
      pieces.push({ kind: 'icon', icon: atom.icon, width: atom.width, x, line });
    }
    x += atom.width;
  };
  const newLine = () => { line += 1; x = 0; };

  const flushWord = () => {
    if (!word.length) return;
    const space = x > 0 ? pendingSpace : undefined;
    pendingSpace = undefined;
    const needed = wordWidth + (space?.width ?? 0);
    if (x > 0 && x + needed > maxWidth) newLine();
    else if (space) place({ kind: 'text', text: ' ', style: space.style, width: space.width });
    if (wordWidth <= maxWidth) {
      word.forEach(place);
    } else {
      for (const atom of word) {
        if (atom.kind === 'icon') {
          if (x > 0 && x + atom.width > maxWidth) newLine();
          place(atom);
          continue;
        }
        for (const char of Array.from(atom.text)) {
          const width = measure(char, atom.style);
          if (x > 0 && x + width > maxWidth) newLine();
          place({ kind: 'text', text: char, style: atom.style, width });
        }
      }
    }
    word = [];
    wordWidth = 0;
  };

  for (const token of tokens) {
    if ('icon' in token) {
      word.push({ kind: 'icon', icon: token.icon, width: token.width });
      wordWidth += token.width;
      lastWasSpace = false;
      continue;
    }
    let run = '';
    const closeRun = () => {
      if (!run) return;
      const width = measure(run, token.style);
      word.push({ kind: 'text', text: run, style: token.style, width });
      wordWidth += width;
      run = '';
    };
    for (const char of Array.from(token.text)) {
      if (isSpace(char)) {
        closeRun();
        flushWord();
        if (!lastWasSpace) pendingSpace = { style: token.style, width: measure(' ', token.style) };
        lastWasSpace = true;
      } else {
        run += char;
        lastWasSpace = false;
      }
    }
    closeRun();
  }
  flushWord();
  return { pieces, lineCount: line + 1 };
}
