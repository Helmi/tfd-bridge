import type { WorldPoint } from '../types';
import type { RpfDirection } from './rpf';

type Bounds = {minX:number; minY:number; maxX:number; maxY:number};

export interface ClippedRpfSector {
  polygon: WorldPoint[];
  /** Only the two bearing boundaries; the rectangle perimeter is not outlined. */
  rays: Array<[WorldPoint, WorldPoint]>;
}

const cross = (a:WorldPoint, b:WorldPoint) => a.x*b.y - a.y*b.x;

function bearingVector(bearing:number):WorldPoint {
  const radians = (((bearing % 360) + 360) % 360) * Math.PI / 180;
  const clean = (value:number) => Math.abs(value) < Number.EPSILON*32 ? 0 : value;
  // WorldPoint follows the map's y-down convention; north decreases y.
  return {x:clean(Math.sin(radians)), y:clean(-Math.cos(radians))};
}

/** Clip a clockwise sector of at most 180 degrees directly to the map bounds. */
export function clipRpfSector(origin:WorldPoint, bounds:Bounds, direction:RpfDirection):ClippedRpfSector {
  const {minX,minY,maxX,maxY} = bounds;
  const span = direction.endBearing - direction.startBearing;
  if (![origin.x,origin.y,minX,minY,maxX,maxY,direction.startBearing,direction.endBearing].every(Number.isFinite)
      || minX >= maxX || minY >= maxY || origin.x < minX || origin.x > maxX || origin.y < minY || origin.y > maxY
      || span < 0 || span > 180) return {polygon:[],rays:[]};

  const epsilon = Math.max(maxX-minX,maxY-minY)*Number.EPSILON*64;
  const start = bearingVector(direction.startBearing);
  const end = bearingVector(direction.endBearing);
  const clampPoint = (point:WorldPoint):WorldPoint => ({
    x:Math.min(maxX,Math.max(minX,point.x)), y:Math.min(maxY,Math.max(minY,point.y)),
  });
  const onBounds = (point:WorldPoint):WorldPoint => {
    const clipped = clampPoint(point);
    if (Math.abs(clipped.x-minX) <= epsilon) clipped.x = minX;
    else if (Math.abs(clipped.x-maxX) <= epsilon) clipped.x = maxX;
    if (Math.abs(clipped.y-minY) <= epsilon) clipped.y = minY;
    else if (Math.abs(clipped.y-maxY) <= epsilon) clipped.y = maxY;
    return clipped;
  };
  const ray = (vector:WorldPoint):[WorldPoint,WorldPoint] | undefined => {
    const horizontal = vector.x > 0 ? (maxX-origin.x)/vector.x : vector.x < 0 ? (minX-origin.x)/vector.x : Infinity;
    const vertical = vector.y > 0 ? (maxY-origin.y)/vector.y : vector.y < 0 ? (minY-origin.y)/vector.y : Infinity;
    const distance = Math.min(horizontal,vertical);
    if (distance <= epsilon) return undefined; // Outward from the map edge.
    return [{...origin},onBounds({x:origin.x+vector.x*distance,y:origin.y+vector.y*distance})];
  };
  const rays = (span === 0 ? [ray(start)] : [ray(start),ray(end)])
    .filter((value):value is [WorldPoint,WorldPoint] => value !== undefined);
  if (span === 0) return {polygon:[],rays};

  // Intersect the rectangle with the halfplane clockwise of the start ray and
  // counter-clockwise of the end ray. This includes intervening map corners
  // without constructing an arbitrarily large triangle beyond the map.
  let polygon:WorldPoint[] = [
    {x:minX,y:minY},{x:maxX,y:minY},{x:maxX,y:maxY},{x:minX,y:maxY},
  ];
  for (const [vector,sign] of [[start,1],[end,-1]] as const) {
    const output:WorldPoint[] = [];
    const distance = (point:WorldPoint) => sign*cross(vector,{x:point.x-origin.x,y:point.y-origin.y});
    for (let i=0;i<polygon.length;i++) {
      const a = polygon[i], b = polygon[(i+1)%polygon.length];
      const da = distance(a), db = distance(b);
      const aInside = da >= -epsilon, bInside = db >= -epsilon;
      if (aInside) output.push(a);
      if (aInside !== bInside) {
        const fraction = Math.min(1,Math.max(0,da/(da-db)));
        output.push(onBounds({x:a.x+(b.x-a.x)*fraction,y:a.y+(b.y-a.y)*fraction}));
      }
    }
    polygon = output;
  }
  const samePoint = (a:WorldPoint,b:WorldPoint) => Math.abs(a.x-b.x) <= epsilon && Math.abs(a.y-b.y) <= epsilon;
  polygon = polygon.filter((point,index,points) => !samePoint(point,points[(index+points.length-1)%points.length]));
  const doubleArea = polygon.reduce((area,point,index) => {
    const next = polygon[(index+1)%polygon.length];
    return area + cross({x:point.x-origin.x,y:point.y-origin.y},{x:next.x-origin.x,y:next.y-origin.y});
  },0);
  if (polygon.length < 3 || Math.abs(doubleArea) <= epsilon*epsilon) polygon = [];
  return {polygon,rays};
}
