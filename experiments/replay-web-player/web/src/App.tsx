import { useEffect, useRef, useState } from 'react';
import { ReplayPicker, type LocalReplaySummary } from './components/ReplayPicker';
import { ResponsiveBroadcast } from './components/ResponsiveBroadcast';
import { advancePlayback, seekPlayback, ENDING_SECONDS } from './engine/ending';
import { RenderProgressDialog } from './components/RenderProgressDialog';
import { VideoSettingsDialog } from './components/VideoSettingsDialog';
import { DEFAULT_LOCAL_VIDEO, type LocalVideoSettings } from './video/renderSettings';
import { sampleScene } from './data/sampleScene';
import { fetchBridgeScene, listBridgeReplays } from './engine/bridgeApi';
import { loadReplayScene } from './engine/importScene';
import { renderAndSave, webCodecsAvailable, type RenderProgress } from './video/encodeMp4';
import { renderInBridge } from './video/bridgeRender';
import { useReplayShare } from './video/shareReplay';
import type { ReplayScene } from './types';

const speeds = [1, 2, 5, 10, 20, 40];

// Picker page size: the bridge returns the newest 30 finalized replays per
// request, so a cold launch only header-reads 30 files. "Load more" pulls the
// next page.
const PAGE_SIZE = 30;

// "Render as video" is still a prototype (td-18bfca) — hidden until it ships.
// Flip to true (with WebCodecs available) to bring the button back.
const SHOW_RENDER_VIDEO = true;

// Set by vite.config.ts only for the `build:bridge` mode: talk to the
// bridge's /player/api/replays + /v1/replays routes instead of the vite-dev
// experiment's /api/* middleware (see src/engine/bridgeApi.ts). The vite-dev
// path below is otherwise unchanged.
const isBridge = import.meta.env.VITE_BRIDGE === '1';

function formatClock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  return `${Math.floor(whole / 60).toString().padStart(2, '0')}:${(whole % 60).toString().padStart(2, '0')}`;
}

function readableName(value: string): string {
  const segments = value.split(/[\\/]/);
  return (segments[segments.length - 1] ?? value)
    .replace(/^\d+_(?:(?:NE|OC)_)?/i, '')
    .replace(/[-_]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

async function fetchGeneratedScene(cacheKey = ''): Promise<ReplayScene> {
  const sceneUrl = new URL(`${import.meta.env.BASE_URL}generated/scene.json${cacheKey ? `?v=${cacheKey}` : ''}`, window.location.href).href;
  const response = await fetch(sceneUrl, { cache: 'no-store' });
  if (!response.ok) throw new Error(`Replay scene could not be loaded (HTTP ${response.status}).`);
  return loadReplayScene(await response.json(), { baseUrl: sceneUrl });
}

// The bridge/engine brand mark (teal ship-wheel), matching the app title bar.
function BrandLogo({ size = 26 }: { size?: number }) {
  return (
    <svg className="brand-logo" viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="#2fd6a6" strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 10.189V14" />
      <path d="M12 2v3" />
      <path d="M19 13V7a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v6" />
      <path d="M19.38 20A11.6 11.6 0 0 0 21 14l-8.188-3.639a2 2 0 0 0-1.624 0L3 14a11.6 11.6 0 0 0 2.81 7.76" />
      <path d="M2 21c.6.5 1.2 1 2.5 1 2.5 0 2.5-2 5-2 1.3 0 1.9.5 2.5 1s1.2 1 2.5 1c2.5 0 2.5-2 5-2 1.3 0 1.9.5 2.5 1" />
    </svg>
  );
}

export function App() {
  // The bridge opens on NO scene and shows the picker straight away (no
  // synthetic/dummy battle). The vite-dev experiment keeps its sample scene.
  const [scene, setScene] = useState<ReplayScene | null>(isBridge ? null : sampleScene);
  const [localReplays, setLocalReplays] = useState<LocalReplaySummary[]>([]);
  const [replaysTotal, setReplaysTotal] = useState(0);
  // The bridge opens straight into the picker, so it starts in a loading state.
  const [listLoading, setListLoading] = useState(isBridge);
  const [loadingMore, setLoadingMore] = useState(false);
  const [listError, setListError] = useState<string>();
  const [pickerOpen, setPickerOpen] = useState(isBridge);
  const [loadingReplayId, setLoadingReplayId] = useState<string>();
  const [localReplayName, setLocalReplayName] = useState<string>();
  const [replayError, setReplayError] = useState<string>();

  useEffect(() => {
    const controller = new AbortController();
    // The bridge has no generated/scene.json (that's a vite-dev-experiment
    // artifact) and starts on NO scene — a replay is chosen from the picker.
    if (!isBridge) {
      void fetchGeneratedScene().then(setScene).catch((reason) => {
        if (!controller.signal.aborted) console.info('Generated scene unavailable; using the synthetic replay.', reason);
      });
    }
    // First page. The bridge path is paginated (newest 30 + total); the vite-dev
    // path returns the whole list at once (no "load more" in the experiment).
    const firstPage: Promise<{ replays: LocalReplaySummary[]; total: number }> = isBridge
      ? listBridgeReplays({ offset: 0, limit: PAGE_SIZE, signal: controller.signal })
      : fetch('/api/replays', { cache: 'no-store', signal: controller.signal })
        .then(async (response) => {
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const replays = (await response.json() as { replays: LocalReplaySummary[] }).replays;
          return { replays, total: replays.length };
        });
    void firstPage
      .then(({ replays, total }) => { setLocalReplays(replays); setReplaysTotal(total); })
      .catch((reason) => {
        if (controller.signal.aborted) return;
        // In the bridge the picker is the only way in, so surface the failure;
        // in the vite-dev experiment the picker is optional, so just log.
        if (isBridge) setListError(reason instanceof Error ? reason.message : String(reason));
        else console.info('Local replay picker is unavailable outside the development experiment.', reason);
      })
      .finally(() => { if (!controller.signal.aborted) setListLoading(false); });
    return () => controller.abort();
  // Initial local-scene discovery only.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // "Load more": append the next page of older replays (bridge only). Dedupes by
  // id in case the folder changed between pages.
  const loadMoreReplays = async () => {
    if (!isBridge || loadingMore || localReplays.length >= replaysTotal) return;
    setLoadingMore(true);
    setListError(undefined);
    try {
      const { replays, total } = await listBridgeReplays({ offset: localReplays.length, limit: PAGE_SIZE });
      setLocalReplays((previous) => {
        const seen = new Set(previous.map((replay) => replay.id));
        return [...previous, ...replays.filter((replay) => !seen.has(replay.id))];
      });
      setReplaysTotal(total);
    } catch (reason) {
      setListError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoadingMore(false);
    }
  };

  const chooseReplay = async (replay: LocalReplaySummary) => {
    if (loadingReplayId) return;
    setLoadingReplayId(replay.id);
    setReplayError(undefined);
    try {
      if (isBridge) {
        // The bridge decodes and returns the scene JSON directly from one GET.
        setScene(await fetchBridgeScene(replay.id));
        setLocalReplayName(replay.id);
      } else {
        const response = await fetch('/api/replays/load', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ id: replay.id }),
        });
        const payload = await response.json() as { error?: string };
        if (!response.ok) throw new Error(payload.error ?? `Replay preparation failed (HTTP ${response.status}).`);
        setScene(await fetchGeneratedScene(Date.now().toString()));
      }
      setPickerOpen(false);
    } catch (reason) {
      setReplayError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoadingReplayId(undefined);
    }
  };

  return (
    <>
      {scene ? (
        // Remount per replay so playback state resets cleanly on a new battle.
        <PlayerView
          key={scene.replay.id}
          scene={scene}
          localReplayName={localReplayName}
          replayCount={replaysTotal || localReplays.length}
          onOpenPicker={() => setPickerOpen(true)}
        />
      ) : (
        <main className="app-shell app-empty">
          <header className="topbar">
            <div className="brand-lockup">
              <BrandLogo />
              <div>
                <div className="eyebrow">TFD RePlayer</div>
                <h1>RePlayer</h1>
              </div>
            </div>
            <button className="choose-replay-button" onClick={() => setPickerOpen(true)}>
              <span>Choose replay</span>
              <small>{replaysTotal || 'Local'}</small>
            </button>
          </header>
          <div className="empty-hero">
            <BrandLogo size={44} />
            <p>Choose a replay from your folder to begin.</p>
            <button className="choose-replay-button" onClick={() => setPickerOpen(true)}>Choose replay</button>
          </div>
        </main>
      )}

      {pickerOpen && (
        <ReplayPicker
          replays={localReplays}
          total={replaysTotal}
          currentFilename={scene?.replay.title ?? ''}
          loadingId={loadingReplayId}
          loading={listLoading}
          loadingMore={loadingMore}
          canLoadMore={isBridge && localReplays.length < replaysTotal}
          pageSize={PAGE_SIZE}
          onLoadMore={loadMoreReplays}
          error={replayError ?? listError}
          onChoose={chooseReplay}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </>
  );
}

function PlayerView({ scene, replayCount, onOpenPicker, localReplayName }: { scene: ReplayScene; replayCount: number; onOpenPicker: () => void; localReplayName?: string }) {
  const share=useReplayShare();
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(10);
  const [selectedShipId, setSelectedShipId] = useState(
    scene.replay.perspectiveEntityId
      ?? scene.ships.find((ship) => ship.relation === 'self')?.id
      ?? scene.ships[0].id,
  );
  const previousFrame = useRef<number | undefined>(undefined);

  // Experimental: render this replay to a 16:9 broadcast-layout mp4 (offline,
  // bridge-local). Prototype — see td-18bfca.
  const [renderProgress, setRenderProgress] = useState<RenderProgress | null>(null);
  const [renderNote, setRenderNote] = useState<string>();
  const [renderComparisons, setRenderComparisons] = useState<{settings:LocalVideoSettings;elapsedMs:number;bytes:number}[]>([]);
  const [videoSettingsOpen, setVideoSettingsOpen] = useState(false);
  const [videoSettings, setVideoSettings] = useState<LocalVideoSettings>(DEFAULT_LOCAL_VIDEO);
  const renderAbort = useRef<AbortController | null>(null);
  useEffect(() => () => renderAbort.current?.abort(), []);
  const sharingRenderActive=Boolean(share.renderProgress);
  useEffect(()=>{if(sharingRenderActive)setPlaying(false);},[sharingRenderActive]);
  const onRenderVideo = async (settings: LocalVideoSettings) => {
    if (renderProgress) return;
    setVideoSettings(settings);
    setVideoSettingsOpen(false);
    setRenderNote(undefined);
    renderAbort.current = new AbortController();
    setPlaying(false);
    setRenderProgress({ frame: 0, total: 1, attempt: 1 });
    const base = (scene.replay.title || perspectiveShip?.shipName || 'replay')
      .replace(/[^\w.-]+/g, '_').slice(0, 60) + '_' + readableName(scene.map.name).replace(/[^\w.-]+/g, '_');
    try {
      const native = isBridge && localReplayName
        ? await renderInBridge(localReplayName,scene.replay.arenaUniqueId,setRenderProgress,renderAbort.current.signal,settings)
        : undefined;
      const started = performance.now();
      const saved = native ?? await renderAndSave(scene, base, (p) => setRenderProgress(p), renderAbort.current.signal,settings);
      setRenderComparisons(runs => [{settings:{...settings},elapsedMs:native ? saved.elapsedMs : performance.now()-started,bytes:saved.bytes},...runs].slice(0,2));
    } catch (reason) {
      setRenderNote(renderAbort.current?.signal.aborted ? 'Video render cancelled.' : `Render failed: ${reason instanceof Error ? reason.message : String(reason)}`);
    } finally {
      setRenderProgress(null);
      renderAbort.current = null;
    }
  };

  useEffect(() => {
    if (!playing) {
      previousFrame.current = undefined;
      return;
    }
    let frame = 0;
    const tick = (now: number) => {
      const previous = previousFrame.current ?? now;
      previousFrame.current = now;
      const delta = Math.min(0.1, (now - previous) / 1000);
      setTime((current) => advancePlayback(current, delta, scene.replay.duration, speed));
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing, scene.replay.duration, speed]);

  useEffect(() => {
    if (time >= scene.replay.duration + ENDING_SECONDS) setPlaying(false);
  }, [scene.replay.duration, time]);

  const perspectiveShip = scene.ships.find(ship => ship.id === scene.replay.perspectiveEntityId) ?? scene.ships.find(ship => ship.relation === 'self');

  const seek = (next: number) => setTime(seekPlayback(next, scene.replay.duration));
  const togglePlayback = () => {
    if (time >= scene.replay.duration + ENDING_SECONDS) setTime(0);
    setPlaying((current) => !current);
  };


  return (
    <main className="app-shell replay-player">
      <header className="topbar">
        <div className="brand-lockup">
          <BrandLogo />
          <h1>RePlayer</h1>
        </div>
        <div className="topbar-actions" aria-label="Replay actions">
          <button className="choose-replay-button" onClick={onOpenPicker}>
            <span>Choose replay</span><small>{replayCount || 'Local'}</small>
          </button>
          <div className="video-actions">
          {isBridge && localReplayName && scene.replay.arenaUniqueId && <button
            disabled={share.busy || Boolean(renderProgress)}
            onClick={()=>{setPlaying(false);void share.start(localReplayName,scene.replay.arenaUniqueId!);}}
            className="share-video-button"
          >Share video</button>}
          {SHOW_RENDER_VIDEO && webCodecsAvailable() && (
            <button
              className="render-video-button"
              onClick={()=>{setPlaying(false);setVideoSettingsOpen(true);}}
              disabled={Boolean(renderProgress) || share.busy}
              title="Save this replay as a local MP4"
            >
              Save video
            </button>
          )}
          </div>
        </div>
      </header>
      {share.label && !share.renderProgress && <div className="render-note" role="status">{share.label}
        {share.authorizationUrl && <> · <a href={share.authorizationUrl} target="_blank" rel="noreferrer">Open Engine authorization</a></>}
        {share.messageUrl && <> · <a href={share.messageUrl} target="_blank" rel="noreferrer">View on Discord</a></>}
      </div>}

      <ResponsiveBroadcast scene={scene} time={time} selectedShipId={selectedShipId} onSelectShip={setSelectedShipId}/>
          <div className="transport">
            <div className="transport-buttons">
              <button onClick={() => seek(Math.min(time, scene.replay.duration) - 10)} aria-label="Back 10 seconds">−10</button>
              <button className="play-button" onClick={togglePlayback} aria-label={playing ? 'Pause' : 'Play'}>{playing ? 'Ⅱ' : '▶'}</button>
              <button onClick={() => seek(time + 10)} aria-label="Forward 10 seconds">+10</button>
            </div>
            <span className="timecode current">{formatClock(Math.min(time, scene.replay.duration))}</span>
            <input
              aria-label="Replay position"
              type="range"
              min="0"
              max={Math.ceil(scene.replay.duration / 0.05) * 0.05}
              step="0.05"
              value={Math.min(time, scene.replay.duration)}
              onChange={(event) => seek(Number(event.target.value))}
              style={{ '--progress': `${Math.min(100, time / scene.replay.duration * 100)}%` } as React.CSSProperties}
            />
            <span className="timecode">{formatClock(scene.replay.duration)}</span>
            <div className="speed-buttons" aria-label="Playback speed">
              {speeds.map((option) => <button key={option} className={speed === option ? 'active' : ''} onClick={() => setSpeed(option)}>{option}×</button>)}
            </div>
          </div>

      {renderNote && <div className="render-note render-result" role="status"><span>{renderNote}</span><button onClick={()=>setRenderNote(undefined)} aria-label="Dismiss video result">×</button></div>}
      {videoSettingsOpen && <VideoSettingsDialog comparisons={renderComparisons} duration={scene.replay.duration} initial={videoSettings} native={isBridge} onClose={()=>setVideoSettingsOpen(false)} onRender={settings=>{void onRenderVideo(settings);}}/>}
      {(renderProgress || share.renderProgress) && <RenderProgressDialog progress={(renderProgress ?? share.renderProgress)!} onCancel={()=>{if(renderProgress)renderAbort.current?.abort();else return share.cancelRender();}}/>}
    </main>
  );
}
