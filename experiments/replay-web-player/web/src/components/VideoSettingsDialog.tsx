import {useEffect, useRef, useState, type CSSProperties, type ReactNode} from 'react';
import {CODEC_LABELS, CODEC_ORDER, detectEncoderSupport, type EncoderChoice, type EncoderSupport, type VideoCodec} from '../video/codecs';
import {BITRATE_OPTIONS_MBPS, estimateLocalVideo, formatFileSize, FPS_OPTIONS, loadVideoDefault, RESOLUTION_OPTIONS, saveVideoDefault, SPEED_OPTIONS, type LocalVideoSettings} from '../video/renderSettings';
import './render-dialog.css';

const ENCODERS: EncoderChoice[] = ['gpu', 'cpu'];
const RESOLUTION_LABELS: Record<LocalVideoSettings['resolution'], [string, string]> = {
  '1080': ['1080p', '1920 × 1080'], '1440': ['1440p', '2560 × 1440'], '2160': ['4K', '3840 × 2160'],
};

/** HEVC is offered only where the machine supports it (or when it is already selected). */
export function visibleCodecs(support: EncoderSupport | undefined, selected: VideoCodec): VideoCodec[] {
  return CODEC_ORDER.filter(c => c !== 'hevc' || c === selected || (support && (support.hevc.gpu || support.hevc.cpu)));
}

/** Keeps a disabled selection usable: same codec on the CPU, else the first enabled button. */
export function settleEncoder(settings: LocalVideoSettings, support: EncoderSupport): LocalVideoSettings {
  const codec = settings.codec ?? 'h264';
  const encoder = settings.encoder ?? 'gpu';
  if (support[codec][encoder]) return settings;
  if (support[codec].cpu) return {...settings, encoder: 'cpu'};
  for (const c of visibleCodecs(support, codec)) for (const e of ENCODERS) if (support[c][e]) return {...settings, codec: c, encoder: e};
  return settings;
}

function Row({label, cols, children}: {label: string; cols: number; children: ReactNode}) {
  return <div className="video-row" role="group" aria-label={label}>
    <span className="video-row-label" aria-hidden="true">{label}</span>
    <div className="video-choices" style={{'--cols': cols} as CSSProperties}>{children}</div>
  </div>;
}

/** The button rows; `support` is undefined until the first probe returns (buttons stay enabled meanwhile). */
export function VideoSettingsFields({batchCount, duration, settings, support, onChange}: {
  batchCount?: number; duration: number; settings: LocalVideoSettings;
  support: EncoderSupport | undefined; onChange: (settings: LocalVideoSettings) => void;
}) {
  const codec = settings.codec ?? 'h264';
  const encoder = settings.encoder ?? 'gpu';
  const codecs = visibleCodecs(support, codec);
  const gpuMissing = support && codecs.some(c => !support[c].gpu);
  const estimate = estimateLocalVideo(duration, settings);
  const seconds = Math.ceil(estimate.seconds);
  return <>
    <Row label="Resolution" cols={3}>{RESOLUTION_OPTIONS.map(r => <button type="button" key={r} aria-pressed={settings.resolution === r} onClick={() => onChange({...settings, resolution: r})}>
      <strong>{RESOLUTION_LABELS[r][0]}</strong><small>{RESOLUTION_LABELS[r][1]}</small></button>)}</Row>
    <Row label="Frame rate" cols={2}>{FPS_OPTIONS.map(n => <button type="button" key={n} aria-pressed={settings.fps === n} onClick={() => onChange({...settings, fps: n})}>{n} fps</button>)}</Row>
    <Row label="Codec" cols={codecs.length * ENCODERS.length}>{codecs.flatMap(c => ENCODERS.map(e =>
      <button type="button" key={`${c}-${e}`} aria-label={`${CODEC_LABELS[c]} · ${e === 'gpu' ? 'GPU' : 'CPU'}`} aria-pressed={codec === c && encoder === e} disabled={support?.[c][e] === false}
        onClick={() => onChange({...settings, codec: c, encoder: e})}><strong>{CODEC_LABELS[c]}</strong><small>{e === 'gpu' ? 'GPU' : 'CPU'}</small></button>))}</Row>
    {gpuMissing && <p className="video-encoder-note">GPU encoding isn't available for these settings here — the CPU exporter is used.</p>}
    <Row label="Bitrate" cols={BITRATE_OPTIONS_MBPS.length}>{BITRATE_OPTIONS_MBPS.map(n => <button type="button" key={n} aria-label={`${n} Mbps`} aria-pressed={settings.bitrate === n * 1_000_000} onClick={() => onChange({...settings, bitrate: n * 1_000_000})}><strong>{n}</strong><small>Mbps</small></button>)}</Row>
    <Row label="Battle speed" cols={5}>{SPEED_OPTIONS.map(n => <button type="button" key={n} aria-pressed={settings.speed === n} onClick={() => onChange({...settings, speed: n})}>{n}×</button>)}</Row>
    {!batchCount && <div className="video-estimate"><strong>{Math.floor(seconds / 60)}:{(seconds % 60).toString().padStart(2, '0')} video</strong><span>· about {formatFileSize(estimate.megabytes)}</span></div>}
  </>;
}

export function VideoSettingsDialog({batchCount, duration, onClose, onRender}: {
  /** Batch variant: one settings object for this many battles; no single-battle estimate. */
  batchCount?: number;
  duration: number;
  onClose: () => void; onRender: (settings: LocalVideoSettings) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [settings, setSettings] = useState(loadVideoDefault); // every open starts from the saved default (the dialog is mounted per open)
  const [support, setSupport] = useState<EncoderSupport>();
  const [saved, setSaved] = useState(false);
  // Live detection for the exact config the encoder would use; never blocks the dialog.
  useEffect(() => {
    let current = true;
    void detectEncoderSupport(settings).then(result => {
      if (!current) return;
      setSupport(result);
      setSettings(s => settleEncoder(s, result));
    });
    return () => {current = false;};
  }, [settings.resolution, settings.fps]);
  useEffect(() => {const element = dialog.current!; element.showModal(); return () => element.close();}, []);
  useEffect(() => {
    if (!saved) return;
    const timer = setTimeout(() => setSaved(false), 2000);
    return () => clearTimeout(timer);
  }, [saved]);
  return <dialog ref={dialog} className="render-dialog video-settings-dialog" aria-labelledby="video-settings-title" onCancel={onClose}>
    <div className="render-dialog-heading">LOCAL VIDEO · MP4</div>
    <h2 id="video-settings-title">{batchCount ? `Save ${batchCount} ${batchCount === 1 ? 'replay' : 'replays'}` : 'Save your replay'}</h2>
    <VideoSettingsFields batchCount={batchCount} duration={duration} settings={settings} support={support} onChange={setSettings}/>
    <div className="render-dialog-footer">
      <button type="button" className="video-default-button" onClick={() => setSaved(saveVideoDefault(settings))}>{saved ? 'Saved as default' : 'Save as my default'}</button>
      <div className="video-footer-actions"><button type="button" onClick={onClose}>Cancel</button><button type="button" className="video-start-button" onClick={() => onRender({...settings, mapSupersampling: true})}>Render & save</button></div>
    </div>
  </dialog>;
}
