export interface RpfDirection {
  /** Degrees clockwise from north, in [0, 360). */
  startBearing: number;
  /** Clockwise sector end. May exceed 360 when crossing north; the sector is
   * strictly positive and at most 180 degrees wide. */
  endBearing: number;
}

const COMPASS_POINTS = ['N', 'NNE', 'NE', 'ENE', 'E', 'ESE', 'SE', 'SSE',
  'S', 'SSW', 'SW', 'WSW', 'W', 'WNW', 'NW', 'NNW'];

/** Parse a complete RPF range announcement such as "RPF: ESE~SE". The ASCII
 * tilde, fullwidth tilde, wave dash, and tilde operator are accepted. Endpoint
 * order is normalized to the shortest clockwise sector; exactly opposite
 * endpoints retain input order because either half-turn is equally short.
 * A single direction or identical endpoints has no stated sector width and
 * is rejected rather than inventing one. */
export function parseRpfDirection(message: string): RpfDirection | null {
  const match = /^\s*RPF\s*:\s*([A-Z]{1,3})\s*[~～〜∼]\s*([A-Z]{1,3})\s*$/i.exec(message);
  if (!match) return null;
  const first = COMPASS_POINTS.indexOf(match[1].toUpperCase());
  const second = COMPASS_POINTS.indexOf(match[2].toUpperCase());
  if (first < 0 || second < 0 || first === second) return null;
  const firstBearing = first * 22.5;
  const secondBearing = second * 22.5;
  const clockwiseWidth = (secondBearing - firstBearing + 360) % 360;
  if (clockwiseWidth <= 180) {
    return {startBearing:firstBearing, endBearing:firstBearing + clockwiseWidth};
  }
  return {startBearing:secondBearing, endBearing:secondBearing + 360 - clockwiseWidth};
}
