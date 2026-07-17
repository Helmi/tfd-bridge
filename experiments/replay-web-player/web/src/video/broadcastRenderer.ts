// Broadcast/observer layout for VIDEO OUTPUT (v7). Biggest possible square map:
// full-height 1080x1080, centred, no border gaps. Flat sidebars (NO cards) with
// proper inside spacing — allied left, enemy right. Each sidebar leads with its
// team's SCORE big, then a roster with short inline HP bars, then a feed (kills
// left, chat right). Bigger type + higher contrast for readability at any size.
//
// TFD Engine tokens (claude.ai/design 527e8989): gold flagship chrome accent, teal
// live/active only, green-black abyss ground, Space Grotesk UI + JetBrains Mono
// numbers, gold eyebrow headers. Team green/red = game data. One Pixi surface;
// driven only by evaluateScene(t). `scale` = renderer resolution (1920x1080 virt).
import { Application, Container, Graphics, Rectangle, Sprite, Text, TextStyle, Texture } from 'pixi.js';
import type { Pose, ReplayScene, SceneState, ShipDefinition, TeamId, TimedValue, WorldPoint } from '../types';
import { evaluateScene } from '../engine/timeline';

type ShipClass = ShipDefinition['shipClass'];

export const VIRTUAL_WIDTH = 1920;
export const VIRTUAL_HEIGHT = 1080;

// ── Composition: full-height square map, flat flanking sidebars ────────────────────
const MAP_SIZE = VIRTUAL_HEIGHT;                         // 1080 — biggest square, no gaps
const MAP = { x: Math.round((VIRTUAL_WIDTH - MAP_SIZE) / 2), y: 0, size: MAP_SIZE };
const SIDE_W = MAP.x;                                    // 420 each flank
const PADX = 24;                                         // sidebar inner side padding
const INNER = SIDE_W - PADX * 2;                         // 372
const LEFT_X = PADX;
const RIGHT_X = MAP.x + MAP.size + PADX;
const TOP = 26, BOTTOM = VIRTUAL_HEIGHT - 26;
const ROW_H = 32;
const HP_W = 78;
const KILL_ROWS = 10;

// ── TFD Engine palette (brightened for contrast) ──────────────────────────────────
const BG = 0x0a1411, TRACK = 0x223a30;
const GOLD = 0xe6b855, TEAL = 0x2fd6a6, COOL = 0x8cb4a8;
const GREEN = 0x67e08c, RED = 0xff6b68, SELF = 0xffd85c, NEUTRAL = 0xaebfd0;
const TEXT = 0xf2f7f4, DIM = 0xc6d6cd, MUTED = 0x93a89e;

const UI = 'Space Grotesk, system-ui, sans-serif';
const MONO = '"JetBrains Mono", ui-monospace, monospace';
const CLASS_CODE: Record<ShipClass, string> = { destroyer: 'DD', cruiser: 'CA', battleship: 'BB', carrier: 'CV', submarine: 'SS' };

interface TextOpts { weight?: '400' | '500' | '600' | '700'; family?: string; letter?: number; shadow?: boolean; }
function makeText(text: string, size: number, color: number, o: TextOpts = {}): Text {
  const style = new TextStyle({ fontFamily: o.family ?? UI, fontSize: size, fill: color, fontWeight: o.weight ?? '400', letterSpacing: o.letter ?? 0 });
  if (o.shadow) style.dropShadow = { color: 0x010507, alpha: 1, blur: 4, distance: 0, angle: 0 };
  return new Text({ text, style });
}
function eyebrow(text: string, color = GOLD, size = 13): Text { return makeText(text.toUpperCase(), size, color, { weight: '700', letter: 3.5 }); }
function num(text: string, size: number, color: number, weight: '400' | '500' | '600' | '700' = '600'): Text { return makeText(text, size, color, { family: MONO, weight }); }

interface Box { minX: number; minY: number; maxX: number; maxY: number; }
function mmss(s: number): string { const x = Math.max(0, Math.round(s)); return `${Math.floor(x / 60)}:${String(x % 60).padStart(2, '0')}`; }
function readable(name: string): string { return name.replace(/^spaces\//, '').replace(/^\d+_/, '').replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase()); }
function clip(s: string, n: number): string { return s.length > n ? `${s.slice(0, n - 1)}…` : s; }

interface RosterRow { text: Text; cls: Text; hpFill: Graphics; hpBg: Graphics; shipId: string; friendly: boolean; y: number; hpX: number; }
interface KillRow { mark: Graphics; victim: Text; killer: Text; y: number; }
interface Death { shipId: string; t: number; killerId?: string; }

export class BroadcastRenderer {
  readonly app: Application;
  private readonly scene: ReplayScene;
  private readonly friendlyTeam: TeamId | undefined;
  private readonly enemyTeam: TeamId | undefined;
  private readonly selfId: string | undefined;
  private view: Box;

  private readonly mapClip = new Container();
  private readonly mapDyn = new Graphics();
  private readonly markerLayer = new Container();

  private readonly scoreFriendly = num('0', 60, GREEN, '700');
  private readonly scoreEnemy = num('0', 60, RED, '700');
  private readonly timerText = num('0:00', 28, TEXT, '600');
  private readonly hpFriendly = num('—', 15, DIM, '600');
  private readonly hpEnemy = num('—', 15, DIM, '600');
  private readonly hpBarFriendly = new Graphics();
  private readonly hpBarEnemy = new Graphics();
  private readonly chatTexts: Text[] = [];
  private readonly killRows: KillRow[] = [];
  private readonly shipMarkers = new Map<string, { chevron: Graphics; label: Text }>();
  private readonly capLabels = new Map<string, Text>();
  private readonly rosterRows: RosterRow[] = [];
  private readonly deaths: Death[] = [];

  private static fontsPromise?: Promise<void>;
  static ensureFonts(): Promise<void> {
    if (!this.fontsPromise) {
      this.fontsPromise = (async () => {
        if (typeof document === 'undefined') return;
        if (!document.getElementById('tfd-fonts')) {
          const link = document.createElement('link');
          link.id = 'tfd-fonts'; link.rel = 'stylesheet';
          link.href = 'https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;600;700&display=swap';
          document.head.appendChild(link);
        }
        try {
          await Promise.all([
            document.fonts.load('700 24px "Space Grotesk"'), document.fonts.load('600 16px "Space Grotesk"'),
            document.fonts.load('400 15px "Space Grotesk"'), document.fonts.load('700 60px "JetBrains Mono"'),
            document.fonts.load('600 16px "JetBrains Mono"'),
          ]);
          await document.fonts.ready;
        } catch { /* best-effort */ }
      })();
    }
    return this.fontsPromise;
  }

  private constructor(app: Application, scene: ReplayScene) {
    this.app = app; this.scene = scene;
    const selfShip = scene.ships.find((s) => s.relation === 'self') ?? scene.ships.find((s) => s.id === scene.replay.perspectiveEntityId);
    this.selfId = selfShip?.id;
    this.friendlyTeam = selfShip?.teamId ?? scene.ships.find((s) => s.relation === 'ally')?.teamId ?? scene.ships[0]?.teamId;
    this.enemyTeam = [...new Set(scene.ships.map((s) => s.teamId))].find((id) => id !== this.friendlyTeam);
    this.view = this.computeActionBounds();
    this.deaths = this.computeDeaths();
  }

  static async create(scene: ReplayScene, opts: { scale?: number } = {}): Promise<BroadcastRenderer> {
    await BroadcastRenderer.ensureFonts();
    const app = new Application();
    await app.init({ width: VIRTUAL_WIDTH, height: VIRTUAL_HEIGHT, resolution: opts.scale ?? 1, autoDensity: false, background: BG, antialias: true, preference: 'webgl', preserveDrawingBuffer: true, autoStart: false } as Parameters<Application['init']>[0]);
    app.ticker.stop();
    const r = new BroadcastRenderer(app, scene);
    await r.build();
    return r;
  }

  get width(): number { return this.app.canvas.width; }
  get height(): number { return this.app.canvas.height; }
  get canvas(): HTMLCanvasElement { return this.app.canvas; }
  private teamColor(t: TeamId | undefined): number { return t === this.friendlyTeam ? GREEN : RED; }

  private computeActionBounds(): Box {
    const { minX, minY, maxX, maxY } = this.scene.map.bounds;
    let ax0 = Infinity, ay0 = Infinity, ax1 = -Infinity, ay1 = -Infinity;
    const c = (p: WorldPoint) => { ax0 = Math.min(ax0, p.x); ay0 = Math.min(ay0, p.y); ax1 = Math.max(ax1, p.x); ay1 = Math.max(ay1, p.y); };
    for (const ship of this.scene.ships) for (const s of ship.pose as TimedValue<Pose>[]) c(s.value);
    for (const cap of this.scene.captureZones) c(cap.center);
    if (!Number.isFinite(ax0)) return { minX, minY, maxX, maxY };
    const px = (ax1 - ax0) * 0.07, py = (ay1 - ay0) * 0.07;
    ax0 -= px; ax1 += px; ay0 -= py; ay1 += py;
    const cx = (ax0 + ax1) / 2, cy = (ay0 + ay1) / 2, half = Math.max((ax1 - ax0) / 2, (ay1 - ay0) / 2);
    let bx0 = cx - half, bx1 = cx + half, by0 = cy - half, by1 = cy + half;
    const shift = (lo: number, hi: number, min: number, max: number): [number, number] => { if (lo < min) { hi += min - lo; lo = min; } if (hi > max) { lo -= hi - max; hi = max; } return [Math.max(lo, min), Math.min(hi, max)]; };
    [bx0, bx1] = shift(bx0, bx1, minX, maxX); [by0, by1] = shift(by0, by1, minY, maxY);
    return { minX: bx0, minY: by0, maxX: bx1, maxY: by1 };
  }

  private computeDeaths(): Death[] {
    const deaths: Death[] = [];
    for (const ship of this.scene.ships) {
      const dead = (ship.health as TimedValue<number>[]).find((v) => v.value <= 0);
      if (!dead) continue;
      let killerId: string | undefined; let best = -Infinity;
      for (const d of this.scene.damage) { if (d.targetId !== ship.id || !d.attackerId) continue; if (d.t <= dead.t + 0.5 && d.t > best) { best = d.t; killerId = d.attackerId; } }
      deaths.push({ shipId: ship.id, t: dead.t, killerId });
    }
    return deaths.sort((a, b) => a.t - b.t);
  }

  private worldToScreen(p: WorldPoint): { x: number; y: number } {
    const v = this.view;
    return { x: MAP.x + ((p.x - v.minX) / (v.maxX - v.minX)) * MAP.size, y: MAP.y + ((p.y - v.minY) / (v.maxY - v.minY)) * MAP.size };
  }

  // ── Build ──────────────────────────────────────────────────────────────────────
  private async build(): Promise<void> {
    const stage = this.app.stage;
    // Map fills its full-height square; a 1px seam sets it off from the flat sidebars.
    const mask = new Graphics().rect(MAP.x, MAP.y, MAP.size, MAP.size).fill({ color: 0xffffff });
    this.mapClip.mask = mask; stage.addChild(mask, this.mapClip);
    await this.loadMap();

    const grid = new Graphics();
    for (let i = 1; i < 8; i++) { const gx = MAP.x + (MAP.size / 8) * i, gy = MAP.y + (MAP.size / 8) * i; grid.moveTo(gx, MAP.y).lineTo(gx, MAP.y + MAP.size); grid.moveTo(MAP.x, gy).lineTo(MAP.x + MAP.size, gy); }
    grid.stroke({ color: TEAL, width: 1, alpha: 0.05 });
    this.mapClip.addChild(grid, this.mapDyn, this.markerLayer);

    const vig = new Graphics();
    vig.rect(MAP.x, MAP.y, MAP.size, 84).fill({ color: 0x02070a, alpha: 0.24 });
    vig.rect(MAP.x, MAP.y + MAP.size - 84, MAP.size, 84).fill({ color: 0x02070a, alpha: 0.22 });
    this.mapClip.addChild(vig);

    const seam = new Graphics();
    seam.rect(MAP.x - 1, 0, 1, VIRTUAL_HEIGHT).fill({ color: GOLD, alpha: 0.16 });
    seam.rect(MAP.x + MAP.size, 0, 1, VIRTUAL_HEIGHT).fill({ color: GOLD, alpha: 0.16 });
    stage.addChild(seam);

    this.buildColumns(stage);
    this.buildMapOverlays(stage);

    for (const cap of this.scene.captureZones) {
      const label = makeText(cap.label ?? '', 28, TEXT, { weight: '700', shadow: true });
      label.anchor.set(0.5); this.capLabels.set(cap.id, label); this.markerLayer.addChild(label);
    }
    for (const ship of this.scene.ships) {
      const chevron = new Graphics();
      const label = makeText(ship.shipName, 13, TEXT, { weight: '600', shadow: true });
      label.anchor.set(0.5, 1); this.shipMarkers.set(ship.id, { chevron, label }); this.markerLayer.addChild(chevron, label);
    }
  }

  private diamond(g: Graphics, cx: number, cy: number, r: number, color: number, alpha = 1): void { g.poly([cx, cy - r, cx + r, cy, cx, cy + r, cx - r, cy]).fill({ color, alpha }); }
  private rule(g: Graphics, x: number, y: number, w: number): void { g.rect(x, y, w, 1).fill({ color: COOL, alpha: 0.14 }); }

  private buildMapOverlays(stage: Container): void {
    const cx = MAP.x + MAP.size / 2;
    const chip = new Graphics();
    chip.roundRect(cx - 62, 16, 124, 40, 8).fill({ color: 0x081310, alpha: 0.85 }).stroke({ color: GOLD, width: 1, alpha: 0.45 });
    stage.addChild(chip);
    this.timerText.anchor.set(0.5); this.timerText.position.set(cx, 37);
    stage.addChild(this.timerText);
  }

  private buildColumns(stage: Container): void {
    const g = new Graphics();
    const teams = new Map<TeamId, string[]>();
    for (const ship of this.scene.ships) { const l = teams.get(ship.teamId) ?? []; l.push(ship.id); teams.set(ship.teamId, l); }

    const buildSidebar = (x0: number, teamId: TeamId | undefined, friendly: boolean) => {
      const ids = teams.get(teamId ?? '') ?? [];
      const team = this.scene.teams.find((t) => t.id === teamId);
      let y = TOP;
      // Header: team eyebrow + map/mode, big score, fleet HP + bar.
      const head = eyebrow(clip(team?.name ?? (friendly ? 'Allied Fleet' : 'Enemy Fleet'), 20), friendly ? GREEN : RED, 15);
      head.position.set(x0, y); stage.addChild(head);
      if (!friendly) { const md = makeText(readable(this.scene.map.name), 13, MUTED, { weight: '500' }); md.anchor.set(1, 0); md.position.set(x0 + INNER, y + 1); stage.addChild(md); }
      const score = friendly ? this.scoreFriendly : this.scoreEnemy;
      score.anchor.set(0, 1); score.position.set(x0, y + 78); stage.addChild(score);
      const hp = friendly ? this.hpFriendly : this.hpEnemy; hp.anchor.set(1, 1); hp.position.set(x0 + INNER, y + 62);
      const hpLbl = eyebrow('Fleet', MUTED, 12); hpLbl.anchor.set(1, 1); hpLbl.position.set(x0 + INNER, y + 44);
      stage.addChild(hp, hpLbl, friendly ? this.hpBarFriendly : this.hpBarEnemy);
      y += 92;
      this.rule(g, x0, y, INNER); y += 14;

      // Roster
      ids.forEach((shipId) => {
        const def = this.scene.ships.find((s) => s.id === shipId)!;
        const cls = makeText(CLASS_CODE[def.shipClass] ?? '··', 11, MUTED, { family: MONO, weight: '600' }); cls.position.set(x0, y + 4);
        const hpX = x0 + INNER - HP_W;
        const label = clip(`${def.shipName}  ${def.clan ? `[${def.clan}] ` : ''}${def.playerName}`, 28);
        const text = makeText(label, 16, friendly ? 0xe4f3ea : 0xffe0df, { weight: '500' }); text.position.set(x0 + 30, y);
        const hpBg = new Graphics(); hpBg.roundRect(hpX, y + 8, HP_W, 5, 2.5).fill({ color: TRACK });
        const hpFill = new Graphics();
        stage.addChild(hpBg, hpFill, cls, text);
        this.rosterRows.push({ text, cls, hpFill, hpBg, shipId, friendly, y, hpX });
        y += ROW_H;
      });
      y += 6; this.rule(g, x0, y, INNER); y += 16;
      return y;
    };

    const leftFeedY = buildSidebar(LEFT_X, this.friendlyTeam, true);
    const rightFeedY = buildSidebar(RIGHT_X, this.enemyTeam, false);

    // Left: KILL FEED. Right: CHAT.
    const kfHead = eyebrow('Kill Feed'); kfHead.position.set(LEFT_X, leftFeedY); stage.addChild(kfHead);
    const kt = leftFeedY + 30;
    for (let i = 0; i < KILL_ROWS; i++) {
      if (kt + i * 34 > BOTTOM) break;
      const y = kt + i * 34; const mark = new Graphics();
      const victim = makeText('', 15.5, TEXT, { weight: '600' }); victim.position.set(LEFT_X + 22, y - 9);
      const killer = makeText('', 13.5, MUTED, { weight: '400' });
      [victim, killer].forEach((t) => { t.visible = false; }); mark.visible = false;
      stage.addChild(mark, victim, killer); this.killRows.push({ mark, victim, killer, y });
    }

    if (rightFeedY < BOTTOM - 40) {
      const chatHead = eyebrow('Chat'); chatHead.position.set(RIGHT_X, rightFeedY); stage.addChild(chatHead);
      const startY = rightFeedY + 30;
      const lines = Math.max(2, Math.floor((BOTTOM - startY) / 24));
      for (let i = 0; i < lines; i++) { const line = makeText('', 15, DIM, { weight: '400' }); line.position.set(RIGHT_X, startY + i * 24); line.visible = false; this.chatTexts.push(line); stage.addChild(line); }
    }

    stage.addChild(g);
  }

  private async loadMap(): Promise<void> {
    const url = this.scene.map.image?.href;
    if (!url) return;
    try {
      const img = new Image(); img.src = url; await img.decode();
      const full = Texture.from(img); const b = this.scene.map.bounds; const iw = img.naturalWidth, ih = img.naturalHeight;
      const fx = ((this.view.minX - b.minX) / (b.maxX - b.minX)) * iw, fy = ((this.view.minY - b.minY) / (b.maxY - b.minY)) * ih;
      const fw = ((this.view.maxX - this.view.minX) / (b.maxX - b.minX)) * iw, fh = ((this.view.maxY - this.view.minY) / (b.maxY - b.minY)) * ih;
      const sub = new Texture({ source: full.source, frame: new Rectangle(fx, fy, fw, fh) });
      const sprite = new Sprite(sub); sprite.position.set(MAP.x, MAP.y); sprite.width = MAP.size; sprite.height = MAP.size;
      this.mapClip.addChildAt(sprite, 0);
    } catch (e) { console.warn('broadcast: map image failed to load', e); }
  }

  // ── Per-frame ────────────────────────────────────────────────────────────────
  renderFrame(t: number): void {
    const state = evaluateScene(this.scene, t);
    this.drawMapDynamic(state);
    this.drawShips(state);
    this.drawHud(state, t);
    this.drawRosters(state);
    this.drawKillFeed(t);
    this.app.render();
  }

  private drawMapDynamic(state: SceneState): void {
    const g = this.mapDyn; g.clear();
    const span = this.view.maxX - this.view.minX;
    for (const cap of state.captureZones) {
      if (!cap.enabled) continue;
      const c = this.worldToScreen(cap.center); const r = (cap.radius / span) * MAP.size;
      const color = cap.owner == null ? NEUTRAL : this.teamColor(cap.owner);
      g.circle(c.x, c.y, r).fill({ color, alpha: 0.07 }).stroke({ color, width: 2.5, alpha: 0.9 });
      if (cap.contested) g.circle(c.x, c.y, r).stroke({ color: GOLD, width: 2.5, alpha: 0.6 });
      const label = this.capLabels.get(cap.definition.id); if (label) { label.position.set(c.x, c.y); label.visible = true; }
    }
    for (const o of state.ordnance) {
      const p = this.worldToScreen(o.position), prev = this.worldToScreen(o.previousPosition);
      if (o.event.kind === 'torpedo') { g.moveTo(prev.x, prev.y).lineTo(p.x, p.y).stroke({ color: o.armed ? 0xffb055 : 0xd0a860, width: 2.5, alpha: 0.9 }); g.circle(p.x, p.y, 2.5).fill({ color: 0xffdca6 }); }
      else g.moveTo(prev.x, prev.y).lineTo(p.x, p.y).stroke({ color: 0xffe9a8, width: 1.75, alpha: 0.55 });
    }
  }

  private drawShips(state: SceneState): void {
    for (const ship of state.ships) {
      const marker = this.shipMarkers.get(ship.definition.id); if (!marker) continue;
      const visible = !ship.destroyed && ship.knowledge !== 'hidden';
      marker.chevron.visible = visible; marker.label.visible = visible; if (!visible) continue;
      const p = this.worldToScreen(ship.displayPose);
      const isSelf = ship.definition.relation === 'self' || ship.definition.id === this.selfId;
      const color = isSelf ? SELF : this.teamColor(ship.definition.teamId);
      const yaw = (ship.displayPose.yaw * Math.PI) / 180, s = 8.5;
      const nose = { x: Math.sin(yaw), y: -Math.cos(yaw) }, side = { x: Math.cos(yaw), y: Math.sin(yaw) };
      const g = marker.chevron; g.clear();
      g.poly([p.x + nose.x * s * 1.5, p.y + nose.y * s * 1.5, p.x - nose.x * s + side.x * s * 0.85, p.y - nose.y * s + side.y * s * 0.85, p.x - nose.x * s - side.x * s * 0.85, p.y - nose.y * s - side.y * s * 0.85]).fill({ color }).stroke({ color: 0x02080a, width: 1.4 });
      marker.label.position.set(p.x, p.y - 11); (marker.label.style as TextStyle).fill = isSelf ? SELF : TEXT;
    }
  }

  private teamHealth(state: SceneState, teamId: TeamId | undefined): number {
    let hp = 0, max = 0;
    for (const ship of state.ships) { if (ship.definition.teamId !== teamId) continue; hp += Math.max(0, ship.health); max += ship.definition.maxHealth; }
    return max ? hp / max : 0;
  }

  private drawHud(state: SceneState, t: number): void {
    this.scoreFriendly.text = String(state.scores[this.friendlyTeam ?? ''] ?? 0);
    this.scoreEnemy.text = String(state.scores[this.enemyTeam ?? ''] ?? 0);
    this.timerText.text = mmss(t);

    const gFrac = this.teamHealth(state, this.friendlyTeam), rFrac = this.teamHealth(state, this.enemyTeam);
    this.hpFriendly.text = `${Math.round(gFrac * 100)}%`; this.hpEnemy.text = `${Math.round(rFrac * 100)}%`;
    const barY = TOP + 74;
    this.hpBarFriendly.clear();
    this.hpBarFriendly.roundRect(LEFT_X, barY, INNER, 6, 3).fill({ color: TRACK });
    this.hpBarFriendly.roundRect(LEFT_X, barY, INNER * gFrac, 6, 3).fill({ color: GREEN });
    this.hpBarEnemy.clear();
    this.hpBarEnemy.roundRect(RIGHT_X, barY, INNER, 6, 3).fill({ color: TRACK });
    this.hpBarEnemy.roundRect(RIGHT_X + INNER * (1 - rFrac), barY, INNER * rFrac, 6, 3).fill({ color: RED });

    if (this.chatTexts.length) {
      const chat = (this.scene.chat ?? []).filter((m) => m.t <= t).slice(-this.chatTexts.length);
      this.chatTexts.forEach((line, i) => { const m = chat[i]; if (!m) { line.visible = false; return; } line.visible = true; line.text = clip(`${m.senderName}: ${m.message}`, 32); });
    }
  }

  private drawRosters(state: SceneState): void {
    const byId = new Map(state.ships.map((s) => [s.definition.id, s]));
    for (const row of this.rosterRows) {
      const ship = byId.get(row.shipId); row.hpFill.clear(); if (!ship) continue;
      const pct = ship.definition.maxHealth ? Math.max(0, ship.health) / ship.definition.maxHealth : 0;
      row.hpFill.roundRect(row.hpX, row.y + 8, Math.max(0, HP_W * pct), 5, 2.5).fill({ color: row.friendly ? GREEN : RED });
      const dead = ship.destroyed; const a = dead ? 0.32 : 1;
      row.text.alpha = a; row.cls.alpha = dead ? 0.3 : 0.85; row.hpBg.alpha = dead ? 0.45 : 1;
    }
  }

  private shipName(id?: string): string { const s = id ? this.scene.ships.find((x) => x.id === id) : undefined; return s?.shipName ?? ''; }

  private drawKillFeed(t: number): void {
    const shown = this.deaths.filter((d) => d.t <= t).slice(-this.killRows.length).reverse();
    this.killRows.forEach((row, i) => {
      const d = shown[i];
      if (!d) { row.mark.visible = row.victim.visible = row.killer.visible = false; return; }
      const victim = this.scene.ships.find((s) => s.id === d.shipId);
      const vColor = victim?.teamId === this.friendlyTeam ? GREEN : RED;
      row.mark.clear(); this.diamond(row.mark, LEFT_X + 6, row.y, 5, vColor);
      row.mark.visible = row.victim.visible = true; (row.victim.style as TextStyle).fill = vColor;
      row.victim.text = victim ? clip(`${victim.shipName} · ${victim.playerName}`, 22) : '—';
      const killer = this.shipName(d.killerId); row.killer.visible = !!killer;
      if (killer) { row.killer.text = `← ${killer}`; row.killer.position.set(row.victim.x + row.victim.width + 8, row.victim.y + 1); }
    });
  }

  destroy(): void { this.app.destroy(true, { children: true }); }
}
