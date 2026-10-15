import {useEffect, useRef, useState} from 'react';
import {CODEC_HINTS, CODEC_LABELS, CODEC_ORDER, CPU_ONLY_HINT, detectCodecSupport, type CodecSupport, type EncodeReport, type VideoCodec} from '../video/codecs';
import {battleLengthSizes, BITRATE_OPTIONS_MBPS, estimateLocalVideo, LOCAL_VIDEO_PRESETS, suggestedBitrates, type LocalVideoSettings} from '../video/renderSettings';
import './render-dialog.css';

export function VideoSettingsDialog({batchCount, duration, initial, native, comparisons = [], onClose, onRender}: {
  /** Batch variant: one settings object for this many battles; no single-battle estimate. */
  batchCount?: number;
  comparisons?: {settings: LocalVideoSettings; elapsedMs: number; bytes: number; encode?: EncodeReport}[];
  duration: number; initial: LocalVideoSettings; native: boolean;
  onClose: () => void; onRender: (settings: LocalVideoSettings) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [settings, setSettings] = useState(initial);
  const estimate = estimateLocalVideo(duration, settings);
  const displaySeconds = Math.ceil(estimate.seconds);
  const codec = settings.codec ?? 'h264';
  const [support, setSupport] = useState<Record<VideoCodec, CodecSupport>>();
  const suggestion = suggestedBitrates(settings.resolution, settings.fps, codec);
  const resolutionLabel = settings.resolution === '2160' ? '4K' : `${settings.resolution}p`;
  const active = LOCAL_VIDEO_PRESETS.find(p => Object.entries(p.settings).every(([key, value]) => settings[key as keyof LocalVideoSettings] === value))?.id;
  // Live detection for the exact config the encoder would use; never blocks the dialog.
  useEffect(() => {
    let current = true;
    void detectCodecSupport(settings).then(result => {
      if (!current) return;
      setSupport(result);
      setSettings(s => result[s.codec ?? 'h264'] === 'unavailable' ? {...s, codec: 'h264'} : s);
    });
    return () => {current = false;};
  }, [settings.resolution, settings.fps, settings.bitrate]);
  useEffect(() => {const element = dialog.current!; element.showModal(); return () => element.close();}, []);
  return <dialog ref={dialog} className="render-dialog video-settings-dialog" aria-labelledby="video-settings-title" onCancel={onClose}>
    <div className="render-dialog-heading">LOCAL VIDEO · MP4</div>
    <h2 id="video-settings-title">{batchCount ? `Save ${batchCount} ${batchCount === 1 ? 'replay' : 'replays'}` : 'Save your replay'}</h2>
    <p>Choose a preset or adjust the settings to compare results.</p>
    <div className="video-presets" aria-label="Video presets">
      {LOCAL_VIDEO_PRESETS.map(p => <button key={p.id} aria-pressed={active === p.id} onClick={() => setSettings({...p.settings,mapSupersampling:settings.mapSupersampling})}>
        <strong>{p.name}</strong><span>{p.description}</span><small>{p.settings.fps} fps · {p.settings.bitrate / 1_000_000} Mbps</small>
      </button>)}
    </div>
    <div className="video-setting-fields">
      <label>Resolution<select value={settings.resolution} onChange={e => setSettings({...settings, resolution:e.target.value as LocalVideoSettings['resolution']})}><option value="1080">1920 × 1080</option><option value="1440">2560 × 1440</option><option value="2160">3840 × 2160</option></select></label>
      <label>Frame rate<select value={settings.fps} onChange={e => setSettings({...settings, fps:Number(e.target.value) as LocalVideoSettings['fps']})}>{[24,30,60].map(n => <option key={n} value={n}>{n} fps</option>)}</select></label>
      <label>Bitrate<select value={settings.bitrate} onChange={e => setSettings({...settings, bitrate:Number(e.target.value)})}>{BITRATE_OPTIONS_MBPS.map(n => <option key={n} value={n * 1_000_000}>{n} Mbps</option>)}</select></label>
      <label>Codec<select value={codec} onChange={e => setSettings({...settings, codec:e.target.value as VideoCodec})}>{CODEC_ORDER.filter(c => c !== 'hevc' || (support && support.hevc !== 'unavailable') || codec === 'hevc').map(c =>
        <option key={c} value={c} disabled={support?.[c] === 'unavailable'}>{CODEC_LABELS[c]}{support && support[c] !== 'unavailable' ? ` · ${support[c] === 'gpu' ? 'GPU' : 'CPU only'}` : ''}</option>)}</select></label>
      <label>Battle speed<select value={settings.speed} onChange={e => setSettings({...settings, speed:Number(e.target.value) as LocalVideoSettings['speed']})}>{[5,10,20].map(n => <option key={n} value={n}>{n}×</option>)}</select></label>
    </div>
    <p className="video-codec-hint">{CODEC_HINTS[codec]}{support?.[codec] === 'cpu' && <> <strong>{CPU_ONLY_HINT}</strong></>}</p>
    <div className="video-suggestion">Suggested for {resolutionLabel} · {settings.fps} fps: {([['Good', suggestion.good], ['Artifact-free', suggestion.artifactFree]] as const).map(([name, mbps]) =>
      <button key={name} aria-pressed={settings.bitrate === mbps * 1_000_000} onClick={() => setSettings({...settings, bitrate:mbps * 1_000_000})}>{name} {mbps} Mbps</button>)}</div>
    <div className="video-length-sizes">Expected size: {battleLengthSizes(settings).map(s => <span key={s.minutes}>{s.minutes} min · {s.label}</span>)}</div>
    <label className="video-antialiasing"><input type="checkbox" checked={settings.mapSupersampling ?? false} onChange={e => setSettings({...settings,mapSupersampling:e.target.checked})}/> Smoother map edges · 2× anti-aliasing</label>
    <p>Renders the map at twice the width and height, then downsamples it. May reduce icon shimmer; uses more rendering time and memory. Output resolution and bitrate stay the same.</p>
    {!batchCount && <div className="video-estimate"><strong>{Math.floor(displaySeconds / 60)}:{(displaySeconds % 60).toString().padStart(2,'0')} video</strong><span>About {Math.round(estimate.megabytes)} MB · actual size varies</span></div>}
    <p className="video-save-location">{native ? <>Saves to your <strong>Videos → TFD Bridge Renders</strong> folder.</> : 'Saves through your browser’s download location.'} {settings.fps === 60 && '60 fps renders twice as many frames as 30 fps.'}</p>
    {!batchCount && comparisons.length > 0 && <section className="video-comparisons"><strong>Recent exports · this replay</strong>{comparisons.map((run,index) => <div key={index}><span>{run.encode && `${CODEC_LABELS[run.encode.codec]} · ${run.encode.hardware ? 'GPU' : 'CPU'} · ${Math.round(run.encode.fps)} fps · `}{run.settings.mapSupersampling ? '2× anti-aliasing' : 'Standard'} · {run.settings.resolution}p · {run.settings.fps} fps · {run.settings.bitrate / 1_000_000} Mbps · {run.settings.speed}×</span><b>{(run.elapsedMs / 1000).toFixed(1)} s · {(run.bytes / 1_048_576).toFixed(1)} MB</b></div>)}<small>Time includes preparation and saving.</small></section>}
    <div className="render-dialog-footer"><button onClick={onClose}>Cancel</button><button className="video-start-button" onClick={() => onRender(settings)}>Render & save</button></div>
  </dialog>;
}
