import {useEffect, useRef, useState} from 'react';
import {estimateLocalVideo, LOCAL_VIDEO_PRESETS, type LocalVideoSettings} from '../video/renderSettings';
import './render-dialog.css';

export function VideoSettingsDialog({duration, initial, native, comparisons = [], onClose, onRender}: {
  comparisons?: {settings: LocalVideoSettings; elapsedMs: number; bytes: number}[];
  duration: number; initial: LocalVideoSettings; native: boolean;
  onClose: () => void; onRender: (settings: LocalVideoSettings) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [settings, setSettings] = useState(initial);
  const estimate = estimateLocalVideo(duration, settings);
  const displaySeconds = Math.ceil(estimate.seconds);
  const active = LOCAL_VIDEO_PRESETS.find(p => Object.entries(p.settings).every(([key, value]) => settings[key as keyof LocalVideoSettings] === value))?.id;
  useEffect(() => {const element = dialog.current!; element.showModal(); return () => element.close();}, []);
  return <dialog ref={dialog} className="render-dialog video-settings-dialog" aria-labelledby="video-settings-title" onCancel={onClose}>
    <div className="render-dialog-heading">LOCAL VIDEO · MP4 / H.264</div>
    <h2 id="video-settings-title">Save your replay</h2>
    <p>Choose a preset or adjust the settings to compare results.</p>
    <div className="video-presets" aria-label="Video presets">
      {LOCAL_VIDEO_PRESETS.map(p => <button key={p.id} aria-pressed={active === p.id} onClick={() => setSettings({...p.settings,mapSupersampling:settings.mapSupersampling})}>
        <strong>{p.name}</strong><span>{p.description}</span><small>{p.settings.fps} fps · {p.settings.bitrate / 1_000_000} Mbps</small>
      </button>)}
    </div>
    <div className="video-setting-fields">
      <label>Resolution<select value={settings.resolution} onChange={e => setSettings({...settings, resolution:e.target.value as LocalVideoSettings['resolution']})}><option value="1080">1920 × 1080</option><option value="1440">2560 × 1440</option></select></label>
      <label>Frame rate<select value={settings.fps} onChange={e => setSettings({...settings, fps:Number(e.target.value) as LocalVideoSettings['fps']})}>{[24,30,60].map(n => <option key={n} value={n}>{n} fps</option>)}</select></label>
      <label>Bitrate<select value={settings.bitrate} onChange={e => setSettings({...settings, bitrate:Number(e.target.value)})}>{[1,2,3,4,5,6,8,10,12].map(n => <option key={n} value={n * 1_000_000}>{n} Mbps</option>)}</select></label>
      <label>Battle speed<select value={settings.speed} onChange={e => setSettings({...settings, speed:Number(e.target.value) as LocalVideoSettings['speed']})}>{[5,10,20].map(n => <option key={n} value={n}>{n}×</option>)}</select></label>
    </div>
    <label className="video-antialiasing"><input type="checkbox" checked={settings.mapSupersampling ?? false} onChange={e => setSettings({...settings,mapSupersampling:e.target.checked})}/> Smoother map edges · 2× anti-aliasing</label>
    <p>Renders the map at twice the width and height, then downsamples it. May reduce icon shimmer; uses more rendering time and memory. Output resolution and bitrate stay the same.</p>
    <div className="video-estimate"><strong>{Math.floor(displaySeconds / 60)}:{(displaySeconds % 60).toString().padStart(2,'0')} video</strong><span>About {Math.round(estimate.megabytes)} MB · actual size varies</span></div>
    <p className="video-save-location">{native ? <>Saves to your <strong>Videos → TFD Bridge Renders</strong> folder.</> : 'Saves through your browser’s download location.'} {settings.fps === 60 && '60 fps renders twice as many frames as 30 fps.'}</p>
    {comparisons.length > 0 && <section className="video-comparisons"><strong>Recent exports · this replay</strong>{comparisons.map((run,index) => <div key={index}><span>{run.settings.mapSupersampling ? '2× anti-aliasing' : 'Standard'} · {run.settings.resolution}p · {run.settings.fps} fps · {run.settings.bitrate / 1_000_000} Mbps · {run.settings.speed}×</span><b>{(run.elapsedMs / 1000).toFixed(1)} s · {(run.bytes / 1_048_576).toFixed(1)} MB</b></div>)}<small>Time includes preparation and saving.</small></section>}
    <div className="render-dialog-footer"><button onClick={onClose}>Cancel</button><button className="video-start-button" onClick={() => onRender(settings)}>Render & save</button></div>
  </dialog>;
}
