import { afterEach, expect, it, vi } from 'vitest';
import { renderInBridge } from './bridgeRender';

afterEach(() => vi.unstubAllGlobals());

it('sends local video preferences, including the encoder, with the render request', async () => {
  const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({id:'local-job'})))
    .mockResolvedValueOnce(new Response(JSON.stringify({state:'rendered',frame:48,total:48,attempt:1,bytes:400_000})));
  vi.stubGlobal('fetch', fetchMock);
  const options = {resolution:'1440' as const,fps:24,bitrate:6_000_000,speed:5,codec:'av1' as const,encoder:'cpu' as const};
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

it('returns the encoder report from the job status video', async () => {
  const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({id:'job'})))
    .mockResolvedValueOnce(new Response(JSON.stringify({state:'rendered',frame:48,total:48,attempt:1,bytes:400_000,
      video:{codec:'av1',hardware:true,render_ms_per_frame:12,wait_ms_per_frame:3,fps:41}})));
  vi.stubGlobal('fetch', fetchMock);
  const result = await renderInBridge('battle.wowsreplay', undefined, vi.fn(), new AbortController().signal, {codec:'av1'});
  expect(JSON.parse(fetchMock.mock.calls[0][1].body).video_options).toEqual({codec:'av1'});
  expect(result?.encode).toEqual({codec:'av1',hardware:true,renderMsPerFrame:12,waitMsPerFrame:3,fps:41});
});

it('maps the encoder_unavailable failure to a pick-another-codec message', async () => {
  const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({id:'job'})))
    .mockResolvedValueOnce(new Response(JSON.stringify({state:'failed',frame:0,total:48,attempt:1,error:'encoder_unavailable'})));
  vi.stubGlobal('fetch', fetchMock);
  await expect(renderInBridge('battle.wowsreplay', undefined, vi.fn(), new AbortController().signal, {encoder:'gpu'}))
    .rejects.toThrow("The selected encoder isn't available for these settings. Pick another codec option.");
});
