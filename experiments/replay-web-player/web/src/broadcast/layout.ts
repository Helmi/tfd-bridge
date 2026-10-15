// Region boxes, z-order, colors and fonts of the 1920x1080 broadcast frame.
// Values are the LAST matching rules in components/broadcast.css (layout.test.ts checks this).
// Inner metrics of a layer live in that layer's own file.

export interface Box { x: number; y: number; w: number; h: number }

export const FRAME = { width: 1920, height: 1080 } as const;

export const Z = { BACKGROUND: 0, MAP: 10, SCOREBOARD: 20, PANEL: 30, ENDING: 40 } as const;

/** `.broadcast` grid columns: map | intel. */
export const MAP_COLUMN_WIDTH = 1080;
export const INTEL_COLUMN_WIDTH = 840;

export const PLAYER_HEIGHT = 220;
export const FEED_HEIGHT = 178;
export const FEED_WINDOW_HEIGHT = 156;
export const ROSTER_ROW_HEIGHT = 44;
export const FLEET_GAP = 28;
export const FLEET_PADDING_TOP = 14;

export const BOX = {
  frame: { x: 0, y: 0, w: 1920, h: 1080 },
  map: { x: 0, y: 0, w: MAP_COLUMN_WIDTH, h: 1080 },
  mapViewport: { x: 27, y: 27, w: 1026, h: 1026 },
  scoreboard: { x: 28, y: 28, w: 1024, h: 105 },
  /** Content box of the intel column: x 1115..1885. */
  intel: { x: 1115, y: 34, w: 770, h: 1024 },
  player: { x: 1115, y: 34, w: 770, h: PLAYER_HEIGHT },
  ribbons: { x: 1115, y: 254, w: 770, h: 80 },
  feed: { x: 1115, y: 334, w: 770, h: FEED_HEIGHT },
  feedWindow: { x: 1115, y: 345, w: 770, h: FEED_WINDOW_HEIGHT },
  fleets: { x: 1115, y: 512, w: 770, h: 546 },
} as const satisfies Record<string, Box>;

/** Two fleet columns side by side, separated by FLEET_GAP. */
export const FLEET_COLUMN_WIDTH = (BOX.fleets.w - FLEET_GAP) / 2;

export const COLORS = {
  muted: '#839b90',
  line: '#26392f',
  green: '#67d9a3',
  red: '#fa8274',
  gold: '#e6b855',
  text: '#edf3ef',
  frameBg: '#0b1711',
  scoreboardBg: '#05110c',
} as const;
export const SCOREBOARD_BG_ALPHA = 0.46;

export const FONT_SANS = "'Space Grotesk', sans-serif";
export const FONT_MONO = "'JetBrains Mono', monospace";
export const FONT_WEIGHTS = [400, 500, 600, 700] as const;

/** Renderer resolution that makes one logical frame pixel one device pixel. */
export function rendererResolution(viewportScale: number, dpr: number): number {
  return viewportScale * dpr;
}

/** Canvas backing size for a resolution (Pixi rounds the same way). */
export function backingSize(resolution: number): { width: number; height: number } {
  return { width: Math.round(FRAME.width * resolution), height: Math.round(FRAME.height * resolution) };
}

/** Client pixels to logical frame coordinates, from the canvas client rect. */
export function toFrame(
  rect: { left: number; top: number; width: number; height: number },
  clientX: number,
  clientY: number,
): { x: number; y: number } {
  return {
    x: (clientX - rect.left) / rect.width * FRAME.width,
    y: (clientY - rect.top) / rect.height * FRAME.height,
  };
}
