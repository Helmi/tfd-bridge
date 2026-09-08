import { describe, expect, it } from 'vitest';
import { budgetBitrate, retryBitrate } from './encodingPolicy';

describe('video upload byte budget', () => {
  it('budgets a full battle with room for the MP4 container', () => {
    const cap = 20 * 1024 * 1024;
    const bitrate = budgetBitrate(cap, 120, 8_000_000);
    expect(bitrate * 120 / 8).toBeLessThanOrEqual(cap * 0.9);
    expect(bitrate).toBeGreaterThan(1_000_000);
    expect(budgetBitrate(cap, 2, 8_000_000)).toBe(8_000_000);
  });
  it('rejects impossible or malformed limits instead of uploading oversized video', () => {
    for (const cap of [NaN, Infinity, -1, 1, 1.5]) expect(() => budgetBitrate(cap, 120, 8_000_000)).toThrow();
    expect(() => budgetBitrate(1024 * 1024, 3600, 8_000_000)).toThrow();
  });
  it('reduces a retry based on measured output size', () => {
    expect(retryBitrate(2_000_000, 30_000_000, 20_000_000)).toBeLessThan(1_200_000);
  });
});
