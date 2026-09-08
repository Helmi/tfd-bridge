import { afterEach, expect, it, vi } from 'vitest';
import { renderInBridge } from './bridgeRender';

afterEach(() => vi.unstubAllGlobals());

it('sends local video preferences with the render request', async () => {
  const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({id:'local-job'})))
    .mockResolvedValueOnce(new Response(JSON.stringify({state:'rendered',frame:48,total:48,attempt:1,bytes:400_000})));
  vi.stubGlobal('fetch', fetchMock);
  const options = {resolution:'1440' as const,fps:24,bitrate:6_000_000,speed:5};
  const result = await renderInBridge('battle.wowsreplay', '9007199254740993', vi.fn(), new AbortController().signal, options);
  expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
    replay_name:'battle.wowsreplay',arena_unique_id:'9007199254740993',video_options:options,
  });
  expect(result).toMatchObject({jobId:'local-job',frames:48,bytes:400_000});
});

it('keeps settings absent for callers using the renderer defaults', async () => {
  const fetchMock = vi.fn().mockResolvedValue(new Response(null, {status:404}));
  vi.stubGlobal('fetch', fetchMock);
  expect(await renderInBridge('battle.wowsreplay', undefined, vi.fn(), new AbortController().signal)).toBeUndefined();
  expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({replay_name:'battle.wowsreplay'});
});
