import type { EncodeReport, VideoCodec } from './codecs';
import type { RenderOptions, RenderProgress, SavedRender } from './encodeMp4';

export async function renderInBridge(replayName: string, arenaId: string | undefined,
  onProgress: (progress: RenderProgress) => void, signal: AbortSignal,
  videoOptions?: Pick<RenderOptions, 'resolution' | 'fps' | 'bitrate' | 'speed' | 'mapSupersampling' | 'codec'>): Promise<SavedRender | undefined> {
  signal.throwIfAborted();
  const started = performance.now();
  const response = await fetch('/v1/render/jobs', {method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify({replay_name:replayName,arena_unique_id:arenaId,video_options:videoOptions})});
  if (!response.ok) {
    const code = await response.json().then((body: {error?: string}) => body.error).catch(() => undefined);
    if (code === 'replay_not_found') throw new Error('Replay file not found.');
    if (code === 'replay_not_ready') throw new Error('Incomplete replay: the end of the battle was not recorded.');
    if (response.status === 404) return undefined; // older Bridge: existing in-page export
    throw new Error(`Could not start replay render (${code ?? response.status})`);
  }
  const initial = await response.json() as {id:string};
  const url = `/v1/render/jobs/${encodeURIComponent(initial.id)}`;
  const cancel = () => {void fetch(`${url}/cancel`,{method:'POST'}).catch(() => undefined);};
  signal.addEventListener('abort',cancel,{once:true});
  try {
    if (signal.aborted) {cancel();signal.throwIfAborted();}
    for (;;) {
      signal.throwIfAborted();
      const result = await fetch(url);
      if (!result.ok) throw new Error('Replay render status unavailable');
      const status = await result.json() as {state:string;frame:number;total:number;attempt:number;bytes?:number;error?:string;
        video?:{codec?:VideoCodec;hardware?:boolean;render_ms_per_frame?:number;wait_ms_per_frame?:number;fps?:number}|null};
      onProgress({frame:status.frame,total:status.total || 1,attempt:status.attempt || 1});
      if (status.state === 'rendered') return {jobId:initial.id,downloadName:`replay-${initial.id}.mp4`,
        elapsedMs:performance.now()-started,frames:status.total,bytes:status.bytes!,
        encode:encodeReport(status.video)};
      if (status.state === 'failed' || status.state === 'cancelled') throw new Error(status.error ?? 'Render cancelled');
      await new Promise(resolve => setTimeout(resolve,750));
    }
  } finally {signal.removeEventListener('abort',cancel);}
}

function encodeReport(video: {codec?:VideoCodec;hardware?:boolean;render_ms_per_frame?:number;wait_ms_per_frame?:number;fps?:number}|null|undefined): EncodeReport|undefined {
  if (!video?.codec || video.hardware === undefined || video.fps === undefined) return undefined;
  return {codec:video.codec,hardware:video.hardware,renderMsPerFrame:video.render_ms_per_frame ?? 0,
    waitMsPerFrame:video.wait_ms_per_frame ?? 0,fps:video.fps};
}
