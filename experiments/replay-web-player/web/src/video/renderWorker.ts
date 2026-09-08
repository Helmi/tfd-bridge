import { loadReplayScene } from '../engine/importScene';
import { renderSceneToMp4, type RenderOptions } from './encodeMp4';

/** Dedicated local Bridge webview. The capability stays in the URL fragment,
 * never the HTTP URL, public job status, or an Engine page. */
export async function runRenderWorker(jobId: string, token: string): Promise<void> {
  const base = `/v1/render/jobs/${encodeURIComponent(jobId)}`;
  const controller = new AbortController();
  let stopped = false;
  let cancelling = false;
  const root = document.getElementById('root')!;
  const post = async (action: string, body?: unknown) => {
    const response = await fetch(`${base}/${action}`, {method:'POST',
      headers:{'Content-Type':'application/json','X-TFD-Render-Worker':token},
      body:JSON.stringify(body ?? {})});
    if (!response.ok) throw new Error(`Worker ${action} failed (${response.status})`);
    return response.json();
  };
  const poll = (async () => {
    while (!stopped) {
      try {
        const response = await fetch(base);
        if (!response.ok) throw new Error('Render job unavailable');
        const status = await response.json();
        if (status.state === 'cancelling' || status.state === 'cancelled') {
          cancelling = true; controller.abort(); break;
        }
      } catch (error) {controller.abort(error); break;}
      await new Promise(resolve => setTimeout(resolve, 750));
    }
  })();
  let progress = Promise.resolve();
  try {
    root.textContent = 'Preparing replay video…';
    const request = await post('claim') as {replay_name:string;arena_unique_id?:string;max_bytes?:number;
      video_options?: Pick<RenderOptions, 'resolution' | 'fps' | 'bitrate' | 'speed' | 'mapSupersampling'> | null};
    const sceneUrl = `/v1/replays/${encodeURIComponent(request.replay_name)}/scene`;
    const response = await fetch(sceneUrl, {signal:controller.signal});
    if (!response.ok) throw new Error('Replay unavailable');
    const scene = loadReplayScene(await response.json(), {baseUrl:new URL(sceneUrl,location.href).href});
    if (request.arena_unique_id && scene.replay.arenaUniqueId !== request.arena_unique_id) throw new Error('Battle identity mismatch');
    const video = await renderSceneToMp4(scene, {...request.video_options,maxBytes:request.max_bytes ?? undefined,signal:controller.signal,
      onProgress: value => {
        root.textContent = `Rendering replay video: ${Math.round(value.frame/value.total*100)}%`;
        progress = progress.then(() => post('progress',value)).then(() => undefined).catch(error => {controller.abort(error);});
      }});
    await progress;
    controller.signal.throwIfAborted();
    await post('metadata', {duration_s:video.frames/video.fps,width:video.width,height:video.height});
    const saved = await fetch(`${base}/video`,{method:'POST',
      headers:{'Content-Type':'video/mp4','X-TFD-Render-Worker':token},body:video.mp4 as BodyInit});
    if (!saved.ok) throw new Error('Saving replay video failed');
    root.textContent = 'Replay video saved.';
  } catch {
    await progress;
    // Re-read after a rejected late progress/save, in case cancellation won.
    try {const response=await fetch(base); const status=await response.json(); cancelling ||= status.state==='cancelling';} catch { /* native owner handles lost workers */ }
    try {await post(cancelling?'stopped':'fail',cancelling?undefined:{error:'render_failed'});} catch { /* native timeout/exit fallback */ }
    root.textContent = cancelling ? 'Render cancelled.' : 'Render failed.';
  } finally {stopped=true; await poll;}
}
