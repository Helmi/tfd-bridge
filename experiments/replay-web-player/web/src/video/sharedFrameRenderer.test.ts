import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-dom', () => ({flushSync: (run: () => void) => run()}));
vi.mock('react-dom/client', () => ({createRoot: vi.fn()}));
vi.mock('../components/BroadcastFrame', () => ({BroadcastFrame: () => null}));
import { SharedFrameRenderer } from './sharedFrameRenderer';

const style = (cssText = '') => ({cssText, setProperty: vi.fn()});

function fixture() {
  const events: string[] = [];
  const mapPanel = {style:style()};
  const canvasClone = {replaceWith:vi.fn()};
  const img = {src:'https://example.test/player/ship.png', currentSrc:'', removeAttribute:vi.fn()};
  const mask = {style:style('mask-image:url("/player/assets/cruiser.svg");color:rgb(1,2,3)')};
  const clone = {
    style:style(), prepend:vi.fn(),
    querySelector: (selector:string) => selector === 'canvas' ? canvasClone : mapPanel,
    querySelectorAll: (selector:string) => selector === 'img' ? [img] : [mask],
  };
  const source = {
    outerHTML:'<div class="broadcast"><div style="transform:translateY(0px)">120</div></div>',
    getBoundingClientRect: () => ({left:-20_000, top:0, width:1920, height:1080}),
    cloneNode:vi.fn(() => clone), querySelector: () => mapPanel,
  };
  const mapCanvas = {
    getBoundingClientRect: () => ({left:-20_000, top:0, width:1080, height:1080}),
    toDataURL: vi.fn(() => {throw new Error('Map must not be PNG-encoded');}),
  };
  const drawImage = vi.fn((input:unknown) => {events.push(input === mapCanvas ? 'map' : 'overlay');});
  const context = {clearRect:vi.fn(), fillRect:vi.fn(), fillStyle:'', drawImage};
  // Exercise capture orchestration without creating a browser/GPU while a user
  // may have a real render running. The DOM/decoder boundaries are explicit fakes.
  const renderer = Object.assign(Object.create(SharedFrameRenderer.prototype), {
    scene:{replay:{duration:100}}, selectedId:'owner', root:{render:vi.fn()},
    host:{querySelector:() => source}, canvas:{width:2560, height:1440, getContext:() => context},
    map:{render:vi.fn(() => mapCanvas)}, resources:new Map(),
    overlayCss:'.broadcast{display:grid}', rootStyle:'display:grid',
    frameBackground:'#0b1711', mapBackground:'#091810',
    dataUrl:vi.fn(async (url:string) => `data:image/png;base64,${btoa(url)}`),
  });
  const decode = vi.fn(async () => {events.push('decode');});
  vi.stubGlobal('Image', class {src=''; decode=decode;});
  vi.stubGlobal('XMLSerializer', class {serializeToString() {return '<div xmlns="http://www.w3.org/1999/xhtml">HUD</div>';}});
  return {renderer, source, clone, mapPanel, canvasClone, mapCanvas, context, img, mask, decode, events};
}

beforeEach(() => {
  vi.stubGlobal('document', {baseURI:'https://example.test/player/', createElement:() => ({style:style(), textContent:''})});
});
afterEach(() => vi.unstubAllGlobals());

describe('shared HTML video capture', () => {
  it('freezes the map at the final battle frame during the ending', async () => {
    const f=fixture();
    await f.renderer.renderFrame(103.5);
    expect(f.renderer.map.render).toHaveBeenCalledWith(100,'owner');
  });
  it('copies the map before awaiting image decoding, scales it, and paints HUD above it', async () => {
    const f = fixture();
    await f.renderer.renderFrame(12.5);
    expect(f.renderer.map.render).toHaveBeenCalledWith(12.5, 'owner');
    expect(f.context.drawImage).toHaveBeenNthCalledWith(1, f.mapCanvas, 0, 0, 1440, 1440);
    expect(f.context.drawImage.mock.calls[1].slice(1)).toEqual([0, 0, 2560, 1440]);
    expect(f.events).toEqual(['map', 'decode', 'overlay']);
    expect(f.mapCanvas.toDataURL).not.toHaveBeenCalled();
    expect(f.clone.style.setProperty).toHaveBeenCalledWith('background', 'transparent', 'important');
    expect(f.mapPanel.style.setProperty).toHaveBeenCalledWith('background', 'transparent', 'important');
    expect(f.canvasClone.replaceWith.mock.calls[0][0].style.cssText).toContain('width:1080px;height:1080px');
    expect(f.img.src).toMatch(/^data:image\/png/);
    expect(f.img.removeAttribute).toHaveBeenCalledWith('srcset');
    expect(f.mask.style.cssText).toContain('color:rgb(1,2,3)');
    expect(f.mask.style.cssText).not.toContain('/player/assets/cruiser.svg');
  });

  it('reuses unchanged HUD pixels but updates on feed motion and backwards seeking', async () => {
    const f = fixture();
    const firstMarkup = f.source.outerHTML;
    await f.renderer.renderFrame(20);
    await f.renderer.renderFrame(20.333);
    expect(f.renderer.map.render).toHaveBeenCalledTimes(2);
    expect(f.source.cloneNode).toHaveBeenCalledTimes(1);
    expect(f.decode).toHaveBeenCalledTimes(1);
    f.source.outerHTML = firstMarkup.replace('translateY(0px)', 'translateY(12px)');
    await f.renderer.renderFrame(20.666);
    expect(f.decode).toHaveBeenCalledTimes(2);
    f.source.outerHTML = firstMarkup;
    await f.renderer.renderFrame(20);
    expect(f.decode).toHaveBeenCalledTimes(3);
    expect(f.renderer.map.render).toHaveBeenLastCalledWith(20, 'owner');
  });

  it('keeps stylesheet order, resolves relative assets, and freezes viewport/media choices', async () => {
    const f = fixture();
    const base = {disabled:false, media:{mediaText:''}};
    Object.assign(document, {styleSheets:[
      {...base, href:'https://example.test/player/assets/first.css', cssRules:[
        {type:1, cssText:'.ship-icon{mask-image:url("./ship.svg");color:red}'},
        {type:4, conditionText:'(max-width:900px)', cssRules:[{type:1, cssText:'.broadcast{width:900px}'}]},
        {type:4, conditionText:'(prefers-reduced-motion:reduce)', cssRules:[{type:1, cssText:'.feed-lines{transform:none}'}]},
      ]},
      {...base, href:'https://example.test/player/second.css', cssRules:[{type:1, cssText:'.ship-icon{color:green}'}]},
      {...base, disabled:true, cssRules:[{type:1, cssText:'.broadcast{display:none}'}]},
    ]});
    vi.stubGlobal('CSSRule', {MEDIA_RULE:4});
    vi.stubGlobal('matchMedia', (query:string) => ({matches:query.includes('reduced-motion')}));
    vi.stubGlobal('getComputedStyle', () => ({
      *[Symbol.iterator]() {yield 'color';}, getPropertyValue:() => '#edf3ef', backgroundColor:'#0b1711',
    }));
    await f.renderer.prepareStyles();
    expect(f.renderer.dataUrl).toHaveBeenCalledWith('https://example.test/player/assets/ship.svg');
    expect(f.renderer.overlayCss.indexOf('color:red')).toBeLessThan(f.renderer.overlayCss.indexOf('color:green'));
    expect(f.renderer.overlayCss).toContain('.feed-lines{transform:none}');
    expect(f.renderer.overlayCss).not.toContain('width:900px');
    expect(f.renderer.overlayCss).not.toContain('display:none');
    expect(f.renderer.overlayCss).toContain('svg:root{background:transparent!important}');
  });
});
