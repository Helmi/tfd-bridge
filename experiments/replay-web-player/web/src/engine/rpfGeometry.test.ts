import {describe,expect,it} from 'vitest';
import type { WorldPoint } from '../types';
import {clipRpfSector} from './rpfGeometry';

const bounds = {minX:0,minY:0,maxX:100,maxY:100};
const origin = {x:50,y:50};
const points = (values:WorldPoint[]) => values.map(p => `${p.x.toFixed(6)},${p.y.toFixed(6)}`).sort();

function contains(polygon:WorldPoint[],point:WorldPoint):boolean {
  if (polygon.length < 3) return false;
  const sides = polygon.map((a,i) => {
    const b = polygon[(i+1)%polygon.length];
    return (b.x-a.x)*(point.y-a.y) - (b.y-a.y)*(point.x-a.x);
  });
  return sides.every(value=>value>=-1e-8) || sides.every(value=>value<=1e-8);
}

describe('RPF map-sector clipping',()=>{
  it('fills the north-east quadrant through the map corner, with only two bearing rays',()=>{
    const result=clipRpfSector(origin,bounds,{startBearing:0,endBearing:90});
    expect(points(result.polygon)).toEqual(points([{x:50,y:0},{x:100,y:0},{x:100,y:50},origin]));
    expect(result.rays).toEqual([[origin,{x:50,y:0}],[origin,{x:100,y:50}]]);
  });

  it('clips diagonal boundaries exactly onto both east corners',()=>{
    const result=clipRpfSector(origin,bounds,{startBearing:45,endBearing:135});
    expect(points(result.polygon)).toEqual(points([origin,{x:100,y:0},{x:100,y:100}]));
    expect(result.rays).toEqual([[origin,{x:100,y:0}],[origin,{x:100,y:100}]]);
  });

  it('handles an unwrapped sector crossing north without filling its opposite side',()=>{
    const result=clipRpfSector(origin,bounds,{startBearing:350,endBearing:370});
    const offset=50*Math.tan(10*Math.PI/180);
    expect(points(result.polygon)).toEqual(points([origin,{x:50-offset,y:0},{x:50+offset,y:0}]));
    expect(contains(result.polygon,{x:50,y:1})).toBe(true);
    expect(contains(result.polygon,{x:50,y:99})).toBe(false);
    expect(result.rays[0][1].y).toBe(0);
    expect(result.rays[1][1].y).toBe(0);
  });

  it('keeps both north corners when a wider north-wrapping sector includes them',()=>{
    const result=clipRpfSector(origin,bounds,{startBearing:315,endBearing:405});
    expect(points(result.polygon)).toEqual(points([origin,{x:0,y:0},{x:100,y:0}]));
  });

  it('supports a 180-degree sector as a half rectangle',()=>{
    const result=clipRpfSector(origin,bounds,{startBearing:270,endBearing:450});
    expect(points(result.polygon)).toEqual(points([{x:0,y:0},{x:100,y:0},{x:100,y:50},{x:0,y:50}]));
    expect(result.rays).toEqual([[origin,{x:0,y:50}],[origin,{x:100,y:50}]]);
  });

  it('includes origins on the boundary, drops outward rays, and rejects outside origins',()=>{
    const edge={x:0,y:50};
    const inward=clipRpfSector(edge,bounds,{startBearing:0,endBearing:90});
    expect(points(inward.polygon)).toEqual(points([{x:0,y:0},{x:100,y:0},{x:100,y:50},edge]));
    expect(inward.rays).toEqual([[edge,{x:0,y:0}],[edge,{x:100,y:50}]]);
    expect(clipRpfSector(edge,bounds,{startBearing:225,endBearing:315})).toEqual({polygon:[],rays:[]});
    expect(clipRpfSector({x:-0.001,y:50},bounds,{startBearing:0,endBearing:90})).toEqual({polygon:[],rays:[]});
  });

  it('treats a zero-width sector as one ray and rejects invalid directions/bounds',()=>{
    expect(clipRpfSector(origin,bounds,{startBearing:90,endBearing:90})).toEqual({polygon:[],rays:[[origin,{x:100,y:50}]]});
    for (const direction of [{startBearing:0,endBearing:181},{startBearing:350,endBearing:10},{startBearing:NaN,endBearing:90}]) {
      expect(clipRpfSector(origin,bounds,direction)).toEqual({polygon:[],rays:[]});
    }
    expect(clipRpfSector(origin,{...bounds,maxX:0},{startBearing:0,endBearing:90})).toEqual({polygon:[],rays:[]});
  });

  it('covers exactly the angular sector on an offset non-square rectangle without overspill',()=>{
    const rectangle={minX:-200,minY:-75,maxX:800,maxY:225};
    const ship={x:-50,y:80};
    for (const [startBearing,endBearing] of [[-30,80],[0,22.5],[337.5,382.5],[45,225],[135,270]]) {
      const result=clipRpfSector(ship,rectangle,{startBearing,endBearing});
      for (const point of result.polygon) {
        expect(point.x).toBeGreaterThanOrEqual(rectangle.minX);
        expect(point.x).toBeLessThanOrEqual(rectangle.maxX);
        expect(point.y).toBeGreaterThanOrEqual(rectangle.minY);
        expect(point.y).toBeLessThanOrEqual(rectangle.maxY);
      }
      for (const [index,[from,to]] of result.rays.entries()) {
        expect(from).toEqual(ship);
        expect(to.x===rectangle.minX || to.x===rectangle.maxX || to.y===rectangle.minY || to.y===rectangle.maxY).toBe(true);
        const actual=Math.atan2(to.x-from.x,from.y-to.y)*180/Math.PI;
        const expected=index===0 ? startBearing : endBearing;
        expect(Math.abs(((actual-expected+540)%360)-180)).toBeLessThan(1e-8);
      }
      // An independent angle-based oracle checks both filled and excluded grid
      // points, catching missing intermediate map corners and opposite wedges.
      for (let x=rectangle.minX;x<=rectangle.maxX;x+=25) for (let y=rectangle.minY;y<=rectangle.maxY;y+=25) {
        const angle=Math.atan2(x-ship.x,ship.y-y)*180/Math.PI;
        const relative=((angle-startBearing)%360+360)%360;
        expect(contains(result.polygon,{x,y})).toBe(relative<=endBearing-startBearing+1e-8);
      }
    }
  });
});
