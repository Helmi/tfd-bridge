import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const fake = vi.hoisted(() => ({load:vi.fn(), encode:vi.fn()}));
vi.mock('../engine/importScene', () => ({loadReplayScene:fake.load}));
vi.mock('./encodeMp4', () => ({renderSceneToMp4:fake.encode}));
import { runRenderWorker } from './renderWorker';

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  vi.stubGlobal('document', {getElementById:() => ({textContent:''})});
  vi.stubGlobal('location', {href:'http://127.0.0.1:43210/player/'});
  fake.load.mockReturnValue({replay:{arenaUniqueId:'9007199254740993'}});
  fake.encode.mockResolvedValue({mp4:new Uint8Array(12),frames:48,fps:24,width:2560,height:1440});
});
afterEach(() => {vi.useRealTimers(); vi.unstubAllGlobals();});

it.each([
  {options:{resolution:'1440',fps:24,bitrate:6_000_000,speed:5,mapSupersampling:true},cap:null},
  {options:null,cap:20_000_000},
])('preserves claimed settings and reports actual encoded dimensions ($options)', async ({options,cap}) => {
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith('/claim')) return new Response(JSON.stringify({replay_name:'battle.wowsreplay',
      arena_unique_id:'9007199254740993',max_bytes:cap,video_options:options}));
    if (url.endsWith('/scene')) return new Response('{}');
    if (!init?.method) return new Response(JSON.stringify({state:'rendering'}));
    return new Response('{}');
  });
  vi.stubGlobal('fetch', fetchMock);
  const work = runRenderWorker('job', 'worker-token');
  await vi.runAllTimersAsync();
  await work;
  expect(fake.encode).toHaveBeenCalledOnce();
  const passed = fake.encode.mock.calls[0][1];
  if (options) expect(passed).toMatchObject(options);
  else for (const key of ['resolution','fps','bitrate','speed']) expect(passed).not.toHaveProperty(key);
  expect(passed.maxBytes).toBe(cap ?? undefined);
  const metadata = fetchMock.mock.calls.find(([url]) => url.endsWith('/metadata'));
  expect(JSON.parse(String(metadata?.[1]?.body))).toEqual({duration_s:2,width:2560,height:1440});
  expect(fetchMock.mock.calls.some(([url]) => url.endsWith('/fail'))).toBe(false);
});

it('streams unbudgeted renders in chunks at their positions, then finishes', async () => {
  fake.encode.mockImplementation(async (_scene: unknown, opts: {sink: {write(d: Uint8Array, p: number): void}}) => {
    opts.sink.write(new Uint8Array([1, 2, 3]), 0);
    opts.sink.write(new Uint8Array([4]), 3);
    opts.sink.write(new Uint8Array([9, 9]), 0);
    return {mp4:new Uint8Array(0),bytes:4,frames:48,fps:24,width:1920,height:1080};
  });
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith('/claim')) return new Response(JSON.stringify({replay_name:'battle.wowsreplay',
      arena_unique_id:'9007199254740993',max_bytes:null,video_options:null}));
    if (url.endsWith('/scene')) return new Response('{}');
    if (!init?.method) return new Response(JSON.stringify({state:'rendering'}));
    return new Response('{}');
  });
  vi.stubGlobal('fetch', fetchMock);
  const work = runRenderWorker('job', 'worker-token');
  await vi.runAllTimersAsync();
  await work;
  const posts = fetchMock.mock.calls.filter(([, init]) => init?.method === 'POST').map(([url, init]) => [url.split('/').pop(), (init!.headers as Record<string, string>)['X-TFD-Chunk-Position']]);
  expect(posts.filter(([a]) => a === 'video-chunk')).toEqual([['video-chunk', '0'], ['video-chunk', '3'], ['video-chunk', '0']]);
  const order = posts.map(([a]) => a);
  expect(order.indexOf('metadata')).toBeLessThan(order.indexOf('video-finish'));
  expect(order.lastIndexOf('video-chunk')).toBeLessThan(order.indexOf('video-finish'));
  expect(order).not.toContain('video');
  expect(order).not.toContain('fail');
});

it('keeps the single video upload when a byte budget is set', async () => {
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith('/claim')) return new Response(JSON.stringify({replay_name:'battle.wowsreplay',
      arena_unique_id:'9007199254740993',max_bytes:20_000_000,video_options:null}));
    if (url.endsWith('/scene')) return new Response('{}');
    if (!init?.method) return new Response(JSON.stringify({state:'rendering'}));
    return new Response('{}');
  });
  vi.stubGlobal('fetch', fetchMock);
  const work = runRenderWorker('job', 'worker-token');
  await vi.runAllTimersAsync();
  await work;
  expect(fake.encode.mock.calls.at(-1)![1].sink).toBeUndefined();
  const urls = fetchMock.mock.calls.map(([url]) => url.split('/').pop());
  expect(urls).toContain('video');
  expect(urls).not.toContain('video-chunk');
  expect(urls).not.toContain('video-finish');
});

it('sends encoder diagnostics with the metadata', async () => {
  fake.encode.mockResolvedValue({mp4:new Uint8Array(12),frames:48,fps:24,width:1920,height:1080,
    encode:{codec:'av1',hardware:false,renderMsPerFrame:12,waitMsPerFrame:3,fps:41}});
  vi.spyOn(console, 'info').mockImplementation(() => undefined);
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith('/claim')) return new Response(JSON.stringify({replay_name:'battle.wowsreplay',
      arena_unique_id:'9007199254740993',max_bytes:20_000_000,video_options:null}));
    if (url.endsWith('/scene')) return new Response('{}');
    if (!init?.method) return new Response(JSON.stringify({state:'rendering'}));
    return new Response('{}');
  });
  vi.stubGlobal('fetch', fetchMock);
  const work = runRenderWorker('job', 'worker-token');
  await vi.runAllTimersAsync();
  await work;
  const metadata = fetchMock.mock.calls.find(([url]) => url.endsWith('/metadata'));
  expect(JSON.parse(String(metadata?.[1]?.body))).toEqual({duration_s:2,width:1920,height:1080,
    codec:'av1',hardware:false,render_ms_per_frame:12,wait_ms_per_frame:3,fps:41});
});
