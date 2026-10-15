import { expect, it } from 'vitest';
import { clock } from './time';

it('formats whole seconds as mm:ss', () => {
  expect(clock(0)).toBe('00:00');
  expect(clock(59.9)).toBe('00:59');
  expect(clock(754)).toBe('12:34');
});
