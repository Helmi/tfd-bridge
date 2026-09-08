/** Reserve room for muxing and encoder variation; actual output is checked too. */
export function budgetBitrate(maxBytes: number, duration: number, preferred: number): number {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 128 * 1024) throw new Error('Invalid video byte limit');
  if (!Number.isFinite(duration) || duration <= 0) throw new Error('Invalid video duration');
  const payload = maxBytes - Math.max(64 * 1024, Math.ceil(maxBytes * 0.1));
  const bitrate = Math.floor(payload * 8 / duration);
  if (bitrate < 128_000) throw new Error('Video is too long for this upload limit');
  return Math.min(preferred, bitrate);
}

export function retryBitrate(previous: number, actualBytes: number, maxBytes: number): number {
  return Math.floor(previous * Math.min(0.85, maxBytes / actualBytes * 0.85));
}
