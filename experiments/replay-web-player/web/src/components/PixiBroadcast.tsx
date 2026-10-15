import { useEffect, useRef, useState } from 'react';
import { BroadcastStage } from '../broadcast/BroadcastStage';
import { rendererResolution } from '../broadcast/layout';
import type { ReplayScene } from '../types';

/** Current devicePixelRatio, updated when it changes (zoom, monitor move). */
function useDevicePixelRatio(): number {
  const [dpr, setDpr] = useState(() => window.devicePixelRatio || 1);
  useEffect(() => {
    const query = matchMedia(`(resolution: ${dpr}dppx)`);
    const onChange = () => setDpr(window.devicePixelRatio || 1);
    query.addEventListener('change', onChange);
    return () => query.removeEventListener('change', onChange);
  }, [dpr]);
  return dpr;
}

/** The broadcast frame: one Pixi canvas, redrawn by one effect per tick. */
export function PixiBroadcast({ scene, time, selectedShipId, onSelectShip, viewportScale }: {
  scene: ReplayScene; time: number; selectedShipId: string; onSelectShip: (id: string) => void;
  /** Scale of the 1920x1080 frame inside its viewport (`ResponsiveBroadcast`). */
  viewportScale: number;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [stage, setStage] = useState<BroadcastStage>();
  const [error, setError] = useState<string>();
  const resolution = rendererResolution(viewportScale, useDevicePixelRatio());
  const latest = useRef({ resolution, onSelectShip });
  latest.current = { resolution, onSelectShip };

  useEffect(() => {
    let cancelled = false;
    let created: BroadcastStage | undefined;
    const initial = latest.current.resolution;
    BroadcastStage.create(scene, { mode: 'interactive', scale: initial > 0 ? initial : 1 }).then(next => {
      if (cancelled) { next.destroy(); return; }
      created = next;
      next.onSelectShip = id => latest.current.onSelectShip(id);
      host.current!.appendChild(next.canvas);
      setStage(next);
    }, (reason: unknown) => { if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason)); });
    return () => { cancelled = true; setStage(undefined); created?.destroy(); };
  }, [scene]);

  useEffect(() => {
    if (!stage || !(resolution > 0)) return;
    stage.setResolution(resolution);
    stage.update(scene, time, selectedShipId);
    stage.render();
  }, [stage, scene, time, selectedShipId, resolution]);

  return <div ref={host} style={{ width: 1920, height: 1080 }}>
    {error ? <div role="alert">Broadcast failed to load: {error}</div> : !stage && <div role="status">Loading broadcast…</div>}
  </div>;
}
