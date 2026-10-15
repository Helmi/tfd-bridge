// Minimal Pixi stand-ins for layer tests. Used through `vi.mock('pixi.js', ...)`.
export class FakePoint {
  x = 0;
  y = 0;
  set(x: number, y = x) { this.x = x; this.y = y; }
}

export class FakeContainer {
  label = '';
  visible = true;
  alpha = 1;
  filters: unknown[] | null = null;
  position = new FakePoint();
  children: FakeContainer[] = [];
  destroyed = false;
  constructor(options?: { label?: string }) { this.label = options?.label ?? ''; }
  get x() { return this.position.x; }
  set x(value: number) { this.position.x = value; }
  get y() { return this.position.y; }
  set y(value: number) { this.position.y = value; }
  addChild(...items: FakeContainer[]) { this.children.push(...items); return items[0]; }
  addChildAt(item: FakeContainer, index: number) { this.children.splice(index, 0, item); return item; }
  destroy() { this.destroyed = true; }
}

export class FakeGraphics extends FakeContainer {
  ops: { op: string; args: unknown[] }[] = [];
  private record(op: string, args: unknown[]) { this.ops.push({ op, args }); return this; }
  clear() { this.ops = []; return this; }
  rect(...args: unknown[]) { return this.record('rect', args); }
  roundRect(...args: unknown[]) { return this.record('roundRect', args); }
  fill(...args: unknown[]) { return this.record('fill', args); }
  stroke(...args: unknown[]) { return this.record('stroke', args); }
}

export interface FakeTexture { width: number; height: number }

export class FakeSprite extends FakeContainer {
  anchor = new FakePoint();
  scale = new FakePoint();
  tint: unknown = 0xffffff;
  rotation = 0;
  texture: FakeTexture;
  constructor(texture: FakeTexture = { width: 1, height: 1 }) { super(); this.texture = texture; }
}

/** Every character is 10 px wide. */
export class FakeText extends FakeContainer {
  anchor = new FakePoint();
  text = '';
  style: Record<string, unknown>;
  constructor(style: Record<string, unknown> = {}) { super(); this.style = { ...style }; }
  get width() { return this.text.length * 10; }
}

export class FakeColorMatrixFilter { matrix: number[] = []; }
export class FakeTextStyle { constructor(public options: unknown) {} }
export const fakeCanvasTextMetrics = { measureText: (value: string) => ({ width: value.length * 10 }) };

/** Flatten a tree to the texts that are really drawn (no hidden ancestor). */
export function visibleTexts(root: FakeContainer): string[] {
  const out: string[] = [];
  const walk = (node: FakeContainer) => {
    if (!node.visible) return;
    if (node instanceof FakeText) out.push(node.text);
    node.children.forEach(walk);
  };
  walk(root);
  return out;
}

export function allOf<T extends FakeContainer>(root: FakeContainer, type: new (...args: never[]) => T): T[] {
  const out: T[] = [];
  const walk = (node: FakeContainer) => {
    if (node instanceof type) out.push(node);
    node.children.forEach(walk);
  };
  walk(root);
  return out;
}
