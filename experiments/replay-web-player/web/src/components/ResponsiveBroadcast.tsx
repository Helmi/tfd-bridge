import { useLayoutEffect, useRef, useState } from 'react';
import { BroadcastFrame } from './BroadcastFrame';
import type { ReplayScene } from '../types';

export function ResponsiveBroadcast(props: {
  scene: ReplayScene; time: number; selectedShipId: string; onSelectShip: (id: string) => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    const element = host.current!;
    const measure = () => setSize({ width: element.clientWidth, height: element.clientHeight });
    const observer = new ResizeObserver(measure);
    measure();
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const scale = Math.min(size.width / 1920, size.height / 1080);
  return <div ref={host} className="broadcast-viewport">
    <div style={{position:'absolute',width:1920,height:1080,left:(size.width-1920*scale)/2,top:(size.height-1080*scale)/2,transform:`scale(${scale})`,transformOrigin:'top left'}}>
      <BroadcastFrame {...props}/>
    </div>
  </div>;
}
