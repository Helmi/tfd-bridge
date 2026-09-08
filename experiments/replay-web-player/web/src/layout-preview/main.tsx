import { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { loadReplayScene } from '../engine/importScene';
import type { ReplayScene, ReplaySceneV1 } from '../types';
import './preview.css';
import { BroadcastFrame } from '../components/BroadcastFrame';

const clock = (t: number) => `${Math.floor(t / 60).toString().padStart(2, '0')}:${Math.floor(t % 60).toString().padStart(2, '0')}`;
function Preview({ scene }: {scene: ReplayScene}) {
  const [time, setTime] = useState(640);
  const [playing, setPlaying] = useState(false);
  const [scale, setScale] = useState(1);
  const [clean, setClean] = useState(false);
  useEffect(() => {
    const resize = () => setScale(Math.min((innerWidth - (clean ? 0 : 48)) / 1920, (innerHeight - (clean ? 0 : 144)) / 1080));
    resize(); window.addEventListener('resize', resize); return () => window.removeEventListener('resize', resize);
  }, [clean]);
  useEffect(() => {
    if (!playing) return;
    let last = performance.now(); let frame = 0;
    const step = (now: number) => { const dt = (now - last) / 1000; last = now; setTime(t => Math.min(scene.replay.duration, t + dt * 10)); frame = requestAnimationFrame(step); };
    frame = requestAnimationFrame(step); return () => cancelAnimationFrame(frame);
  }, [playing, scene]);
  useEffect(() => { const key = (e: KeyboardEvent) => { if (e.key === 'Escape') setClean(false); }; window.addEventListener('keydown', key); return () => window.removeEventListener('keydown', key); }, []);
  return <main className={clean ? 'preview clean' : 'preview'}>
    {!clean && <header className="preview-heading"><div><b>Broadcast layout</b><span>HTML / CSS · existing replay renderer · original game assets</span></div><button onClick={() => setClean(true)}>Frame only ↗</button></header>}
    <div className="frame-space" style={{width:1920*scale,height:1080*scale}}><div style={{transform:`scale(${scale})`,transformOrigin:'top left'}}><BroadcastFrame scene={scene} time={time}/></div></div>
    {!clean && <footer className="preview-tools"><div className="transport"><button onClick={() => setPlaying(p => !p)}>{playing ? 'Pause' : 'Play'} · 10×</button><time>{clock(time)}</time><input aria-label="Replay time" type="range" min="0" max={scene.replay.duration} step="0.1" value={time} onChange={e => {setPlaying(false); setTime(Number(e.target.value));}}/><time>{clock(scene.replay.duration)}</time></div><p>Real Sicilia replay: map, fleets, HP, score, kills and chat. Ribbons and damage counters follow the recorded owner timeline. A dash means the timeline is unavailable.</p></footer>}
  </main>;
}

async function start() {
  const url = new URL('/generated/timeline-scene.json', location.href).href;
  const response = await fetch(url); if (!response.ok) throw new Error(`Replay fixture: HTTP ${response.status}`);
  const wire = await response.json() as ReplaySceneV1;
  const scene = loadReplayScene(wire, {baseUrl:url});
  createRoot(document.getElementById('root')!).render(<Preview scene={scene}/>);
}
void start().catch(e => { document.getElementById('root')!.textContent = `Cannot load layout preview: ${String(e)}`; });
