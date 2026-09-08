import { createRoot, type Root } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { BroadcastFrame } from '../components/BroadcastFrame';
import type { TacticalMapCapture } from '../components/TacticalMap';
import type { ReplayScene } from '../types';

export const VIRTUAL_HEIGHT = 1080;

/** Rasterize the actual replay component. No second implementation of its HUD. */
export class SharedFrameRenderer {
  readonly canvas = document.createElement('canvas');
  private readonly host = document.createElement('div');
  private readonly root: Root;
  private map!: TacticalMapCapture;
  private readonly resources = new Map<string, Promise<string>>();
  private overlayCss = '';
  private rootStyle = '';
  private frameBackground = '';
  private mapBackground = '';
  private overlayMarkup?: string;
  private overlayImage?: HTMLImageElement;
  private readonly selectedId: string;

  private constructor(private scene: ReplayScene, scale: number, private mapResolution = scale) {
    this.canvas.width = Math.round(1920 * scale);
    this.canvas.height = Math.round(1080 * scale);
    this.host.style.cssText = 'position:fixed;left:-20000px;top:0;width:1920px;height:1080px;pointer-events:none';
    this.host.setAttribute('aria-hidden', 'true');
    document.body.appendChild(this.host);
    this.root = createRoot(this.host);
    this.selectedId = scene.replay.perspectiveEntityId ?? scene.ships.find(s => s.relation === 'self')?.id ?? scene.ships[0].id;
  }

  static async create(scene: ReplayScene, opts: {scale?: number; mapSupersampling?: boolean} = {}) {
    const renderer = new SharedFrameRenderer(scene, opts.scale ?? 1, (opts.scale ?? 1) * (opts.mapSupersampling ? 2 : 1));
    try {
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Replay assets did not load within 60 seconds')), 60000);
        renderer.root.render(<BroadcastFrame scene={scene} time={0} selectedShipId={renderer.selectedId}
          mapResolution={renderer.mapResolution} onRendererReady={map => { renderer.map = map; clearTimeout(timeout); resolve(); }}
          onRendererError={error => {clearTimeout(timeout); reject(error);}}/>);
      });
      await document.fonts.ready;
      await renderer.prepareStyles();
      return renderer;
    } catch (error) {renderer.destroy(); throw error;}
  }

  get width() {return this.canvas.width;}
  get height() {return this.canvas.height;}

  private dataUrl(url: string): Promise<string> {
    if (url.startsWith('data:')) return Promise.resolve(url);
    let resource = this.resources.get(url);
    if (!resource) {
      resource = (async () => {
        const response = await fetch(url);
        if (!response.ok) throw new Error(`Video asset failed: HTTP ${response.status}`);
        const blob = await response.blob();
        return new Promise<string>((resolve, reject) => {
          const reader = new FileReader();
          reader.onload = () => resolve(String(reader.result));
          reader.onerror = () => reject(reader.error);
          reader.readAsDataURL(blob);
        });
      })();
      this.resources.set(url, resource);
    }
    return resource;
  }

  private async inlineUrls(css: string, baseUrl = document.baseURI): Promise<string> {
    const matches = [...css.matchAll(/url\(["']?([^"')]+)["']?\)/g)];
    const urls = await Promise.all(matches.map(match => this.dataUrl(new URL(match[1], baseUrl).href)));
    for (let i = 0; i < matches.length; i++) css = css.replace(matches[i][0], `url("${urls[i]}")`);
    return css;
  }

  private async prepareStyles(): Promise<void> {
    // Capture the actual cascade once. Re-reading every computed property on
    // every descendant was substantially more work than updating the replay.
    // Stylesheets also preserve pseudo-elements and reduced-motion rules.
    const rulesAtThisViewport = (rules: CSSRuleList): string => Array.from(rules, rule => {
      if (rule.type === CSSRule.MEDIA_RULE) {
        const media = rule as CSSMediaRule;
        return matchMedia(media.conditionText).matches ? rulesAtThisViewport(media.cssRules) : '';
      }
      return rule.cssText;
    }).join('\n');
    const sheets = await Promise.all(Array.from(document.styleSheets, async sheet => {
      if (sheet.disabled || (sheet.media.mediaText && !matchMedia(sheet.media.mediaText).matches)) return '';
      const css = rulesAtThisViewport(sheet.cssRules);
      return this.inlineUrls(css, sheet.href ?? document.baseURI);
    }));
    this.overlayCss = `${sheets.join('\n')}\nsvg:root{background:transparent!important}.broadcast,.broadcast *{animation:none!important;transition:none!important}`;
    const source = this.host.querySelector<HTMLElement>('.broadcast')!;
    const computed = getComputedStyle(source);
    const style = document.createElement('div').style;
    for (const key of computed) style.setProperty(key, computed.getPropertyValue(key));
    this.rootStyle = await this.inlineUrls(style.cssText);
    this.frameBackground = computed.backgroundColor;
    this.mapBackground = getComputedStyle(source.querySelector('.map-panel')!).backgroundColor;
  }

  private async cloneOverlay(source: HTMLElement, mapWidth: number, mapHeight: number): Promise<HTMLElement> {
    // cloneNode retains current replay-clock text/inline values without reading
    // layout or rebuilding hundreds of computed declarations per element.
    const target = source.cloneNode(true) as HTMLElement;
    target.style.cssText = this.rootStyle;
    target.style.setProperty('background', 'transparent', 'important');
    target.querySelector<HTMLElement>('.map-panel')!.style.setProperty('background', 'transparent', 'important');
    const placeholder = document.createElement('div');
    placeholder.style.cssText = `display:block;width:${mapWidth}px;height:${mapHeight}px`;
    target.querySelector('canvas')!.replaceWith(placeholder);
    await Promise.all([
      ...Array.from(target.querySelectorAll('img'), async img => {
        img.src = await this.dataUrl(img.currentSrc || img.src);
        img.removeAttribute('srcset');
      }),
      ...Array.from(target.querySelectorAll<HTMLElement>('[style]'), async node => {
        if (node.style.cssText.includes('url(')) node.style.cssText = await this.inlineUrls(node.style.cssText);
      }),
    ]);
    return target;
  }

  async renderFrame(time: number): Promise<void> {
    flushSync(() => this.root.render(<BroadcastFrame scene={this.scene} time={time} selectedShipId={this.selectedId} mapResolution={this.mapResolution}/>));
    const source = this.host.querySelector<HTMLElement>('.broadcast')!;
    const bounds = source.getBoundingClientRect();
    const context = this.canvas.getContext('2d')!;
    const scaleX = this.width / bounds.width;
    const scaleY = this.height / bounds.height;
    context.clearRect(0, 0, this.width, this.height);
    context.fillStyle = this.frameBackground;
    context.fillRect(0, 0, this.width, this.height);
    // Consume WebGL's drawing buffer before the first await. Draw the original
    // canvas directly, avoiding PNG compression/base64/decode for every frame.
    const mapCanvas = this.map.render(Math.min(time, this.scene.replay.duration), this.selectedId);
    const mapBounds = mapCanvas.getBoundingClientRect();
    const mapX = (mapBounds.left - bounds.left) * scaleX;
    const mapY = (mapBounds.top - bounds.top) * scaleY;
    const mapWidth = mapBounds.width * scaleX;
    const mapHeight = mapBounds.height * scaleY;
    context.fillStyle = this.mapBackground;
    context.fillRect(mapX, mapY, mapWidth, mapHeight);
    context.imageSmoothingEnabled = true;
    context.imageSmoothingQuality = 'high';
    context.drawImage(mapCanvas, mapX, mapY, mapWidth, mapHeight);
    // Most adjacent frames only move the map. Reuse the exact HTML raster until
    // its DOM changes; replay-clock feed offsets/HP/ribbons are part of this key.
    const markup = source.outerHTML;
    let image = this.overlayImage;
    if (!image || this.overlayMarkup !== markup) {
      const frame = await this.cloneOverlay(source, mapBounds.width, mapBounds.height);
      const style = document.createElement('style');
      style.textContent = this.overlayCss;
      frame.prepend(style);
      const xml = new XMLSerializer().serializeToString(frame);
      const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080"><foreignObject width="1920" height="1080">${xml}</foreignObject></svg>`;
      image = new Image();
      image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
      await image.decode();
      this.overlayMarkup = markup;
      this.overlayImage = image;
    }
    context.drawImage(image, 0, 0, this.width, this.height);
  }

  destroy() {this.root.unmount(); this.host.remove(); this.resources.clear(); this.overlayImage = undefined; this.overlayMarkup = undefined;}
}
