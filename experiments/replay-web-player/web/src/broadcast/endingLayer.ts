import { Container, DOMAdapter, Graphics, Sprite, Texture } from 'pixi.js';
import { endingModel } from '../engine/ending';
import { FRAME } from './layout';
import { setTextIfChanged } from './text';
import type { BroadcastFrameState, BroadcastLayer, StageContext } from './types';
import type { ReplayScene } from '../types';

// Values from the former broadcast.css (see git history): the final `.battle-ending` rules.
const DEEP_RGB = [3, 10, 9] as const;
const FOG = [
  // Listed top first, like the CSS. They are drawn bottom first.
  { at: [0.25, 0.4], alpha: 0.48, end: 0.65 },
  { at: [0.75, 0.7], alpha: 0.35, end: 0.6 },
] as const;
const CENTER = { alpha: 0.5, edge: { rgb: DEEP_RGB, alpha: 0.92 } };
const FOG_PIXELS_PER_PIXEL = 0.25;
/** `box-shadow: inset 0 0 230px 70px #02080788`. */
const INSET_SHADOW = { blur: 230, spread: 70, color: 'rgba(2, 8, 7, 0.5333)', texturePixelsPerPixel: 0.25 };
const TITLE = { size: 100, lineHeight: 115, letterSpacing: 8, color: '#f0f7f2' };
const TITLE_SHADOW = { color: '#000000', alpha: 0.6, blur: 40, distance: 4 };
const TITLE_PADDING = 3 * TITLE_SHADOW.blur;

type Rgb = readonly [number, number, number];
const rgba = (rgb: Rgb, alpha: number) => `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${alpha})`;

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

type Fill2d = Pick<CanvasRenderingContext2D, 'save' | 'restore' | 'translate' | 'scale' | 'clearRect' | 'createRadialGradient' | 'fillRect'> & { fillStyle: unknown };

/**
 * Paints the fog the way the CSS stacks it: the centre gradient at the bottom, then the 75%/70% ellipse,
 * then the 25%/40% ellipse. Each is `radial-gradient(ellipse at fx fy)` with the default `farthest-corner` size.
 * It is drawn with canvas gradients and no base fill: Pixi's own FillGradient first fills its canvas with
 * the last stop colour, which would show through the translucent centre gradient and make the fog opaque.
 */
export function paintFog(context: Fill2d, width: number, height: number, rgb: Rgb): void {
  const k = width / FRAME.width;
  context.clearRect(0, 0, width, height);
  const layers = [
    { at: [0.5, 0.5], stops: premultipliedStops({ rgb, alpha: CENTER.alpha }, CENTER.edge, 8) },
    ...[...FOG].reverse().map(({ at, alpha, end }) => ({ at, stops: [
      { offset: 0, color: rgba(rgb, alpha) }, { offset: end, color: rgba(rgb, 0) }, { offset: 1, color: rgba(rgb, 0) },
    ] })),
  ];
  for (const { at, stops } of layers) {
    const cx = FRAME.width * at[0];
    const cy = FRAME.height * at[1];
    const rx = Math.max(cx, FRAME.width - cx);
    const ry = Math.max(cy, FRAME.height - cy);
    const squash = ry / rx;
    context.save();
    context.translate(cx * k, cy * k);
    context.scale(1, squash);
    const gradient = context.createRadialGradient(0, 0, 0, 0, 0, Math.SQRT2 * rx * k);
    for (const stop of stops) gradient.addColorStop(stop.offset, stop.color);
    context.fillStyle = gradient;
    context.fillRect(-cx * k, -cy * k / squash, width, height / squash);
    context.restore();
  }
}

function fogTexture(): { texture: Texture; paint: (rgb: Rgb) => void } {
  const width = Math.round(FRAME.width * FOG_PIXELS_PER_PIXEL);
  const height = Math.round(FRAME.height * FOG_PIXELS_PER_PIXEL);
  const canvas = DOMAdapter.get().createCanvas(width, height);
  const context = canvas.getContext('2d') as CanvasRenderingContext2D;
  const texture = Texture.from(canvas);
  return { texture, paint: rgb => { paintFog(context, width, height, rgb); texture.source.update(); } };
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

  const fog = fogTexture();
  const fogSprite = new Sprite(fog.texture);
  fogSprite.width = FRAME.width;
  fogSprite.height = FRAME.height;
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
  container.addChild(fogSprite, shadow, title, black);

  let fogKey = '';
  function drawFog(rgb: Rgb): void {
    const key = rgb.join();
    if (key === fogKey) return;
    fogKey = key;
    fog.paint(rgb);
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
      fog.texture.destroy(true);
      texture.destroy(true);
    },
  };
}
