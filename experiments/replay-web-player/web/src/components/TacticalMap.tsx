import { useEffect, useRef, useState } from 'react';
import { Application, Text } from 'pixi.js';
import { evaluateScene } from '../engine/timeline';
import { createMapLayer, type MapLayer } from '../broadcast/mapLayer';
import type { ReplayScene, SceneState } from '../types';

interface Props {
  resolution?: number;
  scene: ReplayScene;
  time: number;
  selectedShipId: string;
  onSelectShip: (id: string) => void;
  presentation?: 'interactive' | 'broadcast';
  onRendererReady?: (renderer: TacticalMapCapture) => void;
  onRendererError?: (error: Error) => void;
}

export interface TacticalMapCapture {
  render(time: number, selectedShipId: string): HTMLCanvasElement;
}

interface Runtime {
  app: Application;
  layer: MapLayer;
  scene: ReplayScene;
  latestState: SceneState;
  selectedShipId: string;
  presentation: 'interactive' | 'broadcast';
}

function drawMap(runtime: Runtime): void {
  const { app, layer, scene, latestState, selectedShipId, presentation } = runtime;
  layer.draw({ scene, state: latestState, selectedId: selectedShipId, presentation, width: app.screen.width, height: app.screen.height });
}

export function TacticalMap({ scene, time, selectedShipId, onSelectShip, presentation = 'interactive', resolution, onRendererReady, onRendererError }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const runtimeRef = useRef<Runtime | null>(null);
  const onSelectRef = useRef(onSelectShip);
  const [error, setError] = useState<string>();
  onSelectRef.current = onSelectShip;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let disposed = false;
    const app = new Application();

    const initialize = async () => {
      try {
        await app.init({
          preference: 'webgl',
          resizeTo: host,
          antialias: true,
          autoDensity: true,
          resolution: resolution ?? Math.min(window.devicePixelRatio || 1, 2),
          backgroundAlpha: 0,
        });
        if (disposed) {
          app.destroy(true);
          return;
        }

        app.canvas.className = 'tactical-canvas';
        host.appendChild(app.canvas);
        const layer = createMapLayer({
          renderer: app.renderer,
          resolution: app.renderer.resolution,
          mapSupersampling: false,
          toFrame: (clientX, clientY) => {
            const rect = app.canvas.getBoundingClientRect();
            return {
              x: (clientX - rect.left) * app.screen.width / rect.width,
              y: (clientY - rect.top) * app.screen.height / rect.height,
            };
          },
          createText: style => new Text({ style }),
          onSelect: id => onSelectRef.current(id),
        }, scene);
        app.stage.addChild(layer.container);
        runtimeRef.current = {
          app,
          layer,
          scene,
          selectedShipId,
          presentation,
          latestState: evaluateScene(scene, time),
        };
        drawMap(runtimeRef.current);
        const assetLoading = layer.ready();

        const resizeObserver = new ResizeObserver(() => {
          requestAnimationFrame(() => {
            if (runtimeRef.current) drawMap(runtimeRef.current);
          });
        });
        resizeObserver.observe(host);
        (runtimeRef.current as Runtime & { cleanup?: () => void }).cleanup = () => {
          resizeObserver.disconnect();
        };
        await assetLoading;
        if (!disposed) onRendererReady?.({
          render(frameTime, selection) {
            const runtime = runtimeRef.current;
            if (!runtime) throw new Error('Map renderer was disposed');
            runtime.selectedShipId = selection;
            runtime.latestState = evaluateScene(runtime.scene, frameTime);
            drawMap(runtime);
            app.renderer.render(app.stage);
            return app.canvas;
          },
        });
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
        onRendererError?.(reason instanceof Error ? reason : new Error(String(reason)));
      }
    };

    void initialize();
    return () => {
      disposed = true;
      const runtime = runtimeRef.current as (Runtime & { cleanup?: () => void }) | null;
      if (runtime) {
        runtime.layer.destroy();
      }
      runtime?.cleanup?.();
      runtimeRef.current = null;
      if (app.renderer) app.destroy(true, { children: true });
    };
  // Pixi owns its lifecycle; scene changes are handled by the drawing effect below.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const runtime = runtimeRef.current;
    if (!runtime) return;
    runtime.scene = scene;
    runtime.selectedShipId = selectedShipId;
    runtime.presentation = presentation;
    runtime.latestState = evaluateScene(scene, time);
    drawMap(runtime);
  }, [scene, time, selectedShipId, presentation]);

  return (
    <div className="map-host" ref={hostRef}>
      {error && (
        <div className="renderer-error">
          <strong>The tactical map could not start</strong>
          <span>{error}</span>
        </div>
      )}
    </div>
  );
}
