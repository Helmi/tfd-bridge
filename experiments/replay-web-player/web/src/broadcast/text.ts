import { CanvasTextMetrics, TextStyle, type TextStyleOptions } from 'pixi.js';

export type MeasureText = (value: string, style: TextStyleOptions) => number;

const measureWithPixi: MeasureText = (value, style) => CanvasTextMetrics.measureText(value, new TextStyle(style)).width;

/** Assign text only when it changed: Pixi re-renders the text texture on every assignment. */
export function setTextIfChanged(text: { text: string | number }, value: string): void {
  if (text.text !== value) text.text = value;
}

/**
 * Cut `value` to `maxWidth` and end it with an ellipsis. Non-breaking spaces count as
 * ordinary characters. The ellipsis never follows whitespace.
 */
export function truncateToWidth(value: string, style: TextStyleOptions, maxWidth: number, measure: MeasureText = measureWithPixi): string {
  if (measure(value, style) <= maxWidth) return value;
  const chars = Array.from(value);
  const fits = (count: number) => measure(`${chars.slice(0, count).join('').trimEnd()}…`, style) <= maxWidth;
  let low = 0;
  let high = chars.length;
  while (low < high) {
    const mid = Math.ceil((low + high) / 2);
    if (fits(mid)) low = mid; else high = mid - 1;
  }
  return `${chars.slice(0, low).join('').trimEnd()}…`;
}
