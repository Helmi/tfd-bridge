// Test double for the 2D canvas context used by the ending fog: it records gradient fills
// and composites them at a point, so tests read the effective fog alpha.
interface Stop { offset: number; alpha: number }
class FakeGradient {
  stops: Stop[] = [];
  constructor(public radius: number) {}
  addColorStop(offset: number, color: string) {
    const alpha = Number(/rgba\([^,]+,[^,]+,[^,]+,\s*([\d.]+)\)/.exec(color)?.[1] ?? 1);
    this.stops.push({ offset, alpha });
  }
  alphaAt(distance: number): number {
    const t = Math.min(Math.max(distance / this.radius, 0), 1);
    const next = this.stops.findIndex(s => s.offset >= t);
    if (next <= 0) return this.stops[Math.max(next, 0)].alpha;
    const [a, b] = [this.stops[next - 1], this.stops[next]];
    return b.offset === a.offset ? b.alpha : a.alpha + (b.alpha - a.alpha) * (t - a.offset) / (b.offset - a.offset);
  }
}
interface Fill { style: unknown; rect: [number, number, number, number]; tx: number; ty: number; sy: number }

export class FakeContext {
  fillStyle: unknown = '#000';
  fills: Fill[] = [];
  private transform = { tx: 0, ty: 0, sy: 1 };
  private saved: (typeof this.transform)[] = [];
  save() { this.saved.push({ ...this.transform }); }
  restore() { this.transform = this.saved.pop()!; }
  translate(x: number, y: number) { this.transform.tx += x; this.transform.ty += y; }
  scale(_x: number, y: number) { this.transform.sy *= y; }
  clearRect() { this.fills = []; }
  createRadialGradient(_x0: number, _y0: number, _r0: number, _x1: number, _y1: number, r1: number) { return new FakeGradient(r1); }
  fillRect(x: number, y: number, w: number, h: number) { this.fills.push({ style: this.fillStyle, rect: [x, y, w, h], ...this.transform }); }

  /** Effective fog alpha at a canvas pixel: source-over of every fill, bottom first. */
  alphaAt(px: number, py: number): number {
    let alpha = 0;
    for (const { style, rect: [x, y, w, h], tx, ty, sy } of this.fills) {
      const lx = px - tx;
      const ly = (py - ty) / sy;
      if (lx < x || lx > x + w || ly < y || ly > y + h) continue;
      const source = style instanceof FakeGradient ? style.alphaAt(Math.hypot(lx, ly)) : 1;
      alpha = source + alpha * (1 - source);
    }
    return alpha;
  }
}

export class FakeCanvas {
  context = new FakeContext();
  constructor(public width: number, public height: number) {}
  getContext() { return this.context; }
}
