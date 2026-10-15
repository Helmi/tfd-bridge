import { Container, DOMAdapter, FillGradient, Graphics, Sprite, Texture } from 'pixi.js';
import { endingModel } from '../engine/ending';
import { FRAME } from './layout';
import { setTextIfChanged } from './text';
import type { BroadcastFrameState, BroadcastLayer, StageContext } from './types';
import type { ReplayScene } from '../types';

// The final `.battle-ending` rules in components/broadcast.css.
const DEEP_RGB = [3, 10, 9] as const;
const FOG = [
  // Listed top first, like the CSS. They are drawn bottom first.
  { at: [0.25, 0.4], alpha: 0.48, end: 0.65 },
  { at: [0.75, 0.7], alpha: 0.35, end: 0.6 },
] as const;
const CENTER = { alpha: 0.5, edge: { rgb: DEEP_RGB, alpha: 0.92 } };
const GRADIENT_TEXTURE_SIZE = 512;
/** `box-shadow: inset 0 0 230px 70px #02080788`. */
const INSET_SHADOW = { blur: 230, spread: 70, color: 'rgba(2, 8, 7, 0.5333)', texturePixelsPerPixel: 0.25 };
const TITLE = { size: 100, lineHeight: 115, letterSpacing: 8, color: '#f0f7f2' };
const TITLE_SHADOW = { color: '#000000', alpha: 0.6, blur: 40, distance: 4 };
const TITLE_PADDING = 3 * TITLE_SHADOW.blur;

type Rgb = readonly [number, number, number];
const rgba = (rgb: Rgb, alpha: number) => `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${alpha})`;

/** CSS `radial-gradient(ellipse at fx fy)` with the default `farthest-corner` size. */
function ellipseAt(fx: number, fy: number, colorStops: { offset: number; color: string }[]): FillGradient {
  const center = { x: FRAME.width * fx, y: FRAME.height * fy };
  const rx = Math.max(center.x, FRAME.width - center.x);
  const ry = Math.max(center.y, FRAME.height - center.y);
  // Pixi squashes a circle of the longer radius along y.
  return new FillGradient({
    type: 'radial', center, outerCenter: center, innerRadius: 0, outerRadius: Math.SQRT2 * rx, scale: ry / rx,
    colorStops, textureSpace: 'global', textureSize: GRADIENT_TEXTURE_SIZE,
  });
}

/**
 * Stops that fade from `from` to `to` the way CSS does (premultiplied alpha).
 * Canvas gradients blend the straight colours, which would darken the fog too early.
 */
export function premultipliedStops(from: { rgb: Rgb; alpha: number }, to: { rgb: Rgb; alpha: number }, steps: number): { offset: number; color: string }[] {
  return Array.from({ length: steps + 1 }, (_, i) => {
    const t = i / steps;
    const alpha = from.alpha + (to.alpha - from.alpha) * t;
    const channel = (k: 0 | 1 | 2) => Math.round((from.rgb[k] * from.alpha + (to.rgb[k] * to.alpha - from.rgb[k] * from.alpha) * t) / alpha);
    return { offset: t, color: rgba([channel(0), channel(1), channel(2)], alpha) };
  });
}

function fogGradients(rgb: Rgb): FillGradient[] {
  const center = ellipseAt(0.5, 0.5, premultipliedStops({ rgb, alpha: CENTER.alpha }, CENTER.edge, 8));
  const fades = [...FOG].reverse().map(({ at, alpha, end }) => ellipseAt(at[0], at[1], [
    { offset: 0, color: rgba(rgb, alpha) }, { offset: end, color: rgba(rgb, 0) }, { offset: 1, color: rgba(rgb, 0) },
  ]));
  return [center, ...fades];
}

/** The inset edge shadow, drawn once: a blurred frame around the inner rectangle. Quarter size, it is smooth anyway. */
export function insetShadowTexture(): Texture {
  const k = INSET_SHADOW.texturePixelsPerPixel;
  const width = Math.round(FRAME.width * k);
  const height = Math.round(FRAME.height * k);
  const canvas = DOMAdapter.get().createCanvas(width, height);
  const context = canvas.getContext('2d') as CanvasRenderingContext2D;
  // Only the shadow lands on the canvas: the shape is far to the left, the shadow offset brings it back.
  const far = 10 * width;
  const margin = 2 * INSET_SHADOW.blur * k;
  const inset = INSET_SHADOW.spread * k;
  context.shadowColor = INSET_SHADOW.color;
  context.shadowBlur = INSET_SHADOW.blur * k;
  context.shadowOffsetX = far;
  context.beginPath();
  context.rect(-far - margin, -margin, width + 2 * margin, height + 2 * margin);
  context.rect(-far + inset, inset, width - 2 * inset, height - 2 * inset);
  context.fill('evenodd');
  return Texture.from(canvas);
}

/**
 * The closing screen: fog, edge shadow, title, black fade. It never takes pointer events,
 * so the map stays clickable while it shows. `shadowTexture` is injectable for tests.
 */
export function createEndingLayer(ctx: StageContext, _scene: ReplayScene, shadowTexture: () => Texture = insetShadowTexture): BroadcastLayer {
  const container = new Container({ label: 'ending' });
  container.eventMode = 'none';
  container.visible = false;

  const fog = new Graphics();
  const texture = shadowTexture();
  const shadow = new Sprite(texture);
  shadow.width = FRAME.width;
  shadow.height = FRAME.height;
  const title = ctx.createText({
    fontSize: TITLE.size, fontWeight: '600', lineHeight: TITLE.lineHeight, letterSpacing: TITLE.letterSpacing, fill: TITLE.color,
    align: 'center', padding: TITLE_PADDING,
    dropShadow: { color: TITLE_SHADOW.color, alpha: TITLE_SHADOW.alpha, blur: TITLE_SHADOW.blur, distance: TITLE_SHADOW.distance, angle: Math.PI / 2 },
  });
  title.anchor.set(0.5);
  title.position.set(FRAME.width / 2, FRAME.height / 2);
  const black = new Graphics().rect(0, 0, FRAME.width, FRAME.height).fill(0x000000);
  black.alpha = 0;
  container.addChild(fog, shadow, title, black);

  let gradients: FillGradient[] = [];
  let fogKey = '';
  function drawFog(rgb: Rgb): void {
    const key = rgb.join();
    if (key === fogKey) return;
    fogKey = key;
    fog.clear();
    for (const gradient of gradients) gradient.destroy();
    gradients = fogGradients(rgb);
    for (const gradient of gradients) fog.rect(0, 0, FRAME.width, FRAME.height).fill(gradient);
  }

  return {
    container,
    update(frame: BroadcastFrameState) {
      const model = endingModel(frame.scene.replay, frame.time - frame.scene.replay.duration);
      container.visible = model !== null;
      if (!model) return;
      drawFog(model.rgb);
      setTextIfChanged(title, model.title.toUpperCase());
      if (black.alpha !== model.blackAlpha) black.alpha = model.blackAlpha;
    },
    destroy() {
      container.destroy({ children: true });
      for (const gradient of gradients) gradient.destroy();
      gradients = [];
      texture.destroy(true);
    },
  };
}
