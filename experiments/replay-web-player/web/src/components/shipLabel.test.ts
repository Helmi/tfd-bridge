import { describe, expect, it } from 'vitest';
import { shipLabelLayout } from './shipLabel';
import { isOwnDivision, shipIconColor } from '../divisions';
import { sampleScene } from '../data/sampleScene';

describe('ship name and division prefix layout', () => {
  for (const size of [380,760,1026,1440]) for (const ownDivision of [false,true]) {
    it(`centres and clamps the whole label at map size ${size}, division ${ownDivision}`, () => {
      const opts = {viewport:{left:27,top:27,size},point:{x:27+size/2,y:100},scale:size/760,
        color:'#ffd369',alpha:.56,ownDivision};
      const centred = shipLabelLayout(95,18,opts);
      expect(centred.x + centred.width / 2).toBeCloseTo(opts.point.x);
      expect(centred.y).toBeCloseTo(100+13*opts.scale);
      for (const x of [-10,27,27+size,27+size+10]) for (const y of [-10,27,27+size,27+size+10]) {
        const box = shipLabelLayout(95,18,{...opts,point:{x,y}});
        expect(box.x).toBeGreaterThanOrEqual(27);
        expect(box.x+box.width).toBeLessThanOrEqual(27+size+.00001);
        expect(box.y).toBeGreaterThanOrEqual(27);
        expect(box.y+18).toBeLessThanOrEqual(27+size);
        if (ownDivision) expect(box.nameX-(box.dotX+2.4)).toBeCloseTo(4);
        else expect(box.nameX).toBe(0);
      }
    });
  }
  it('resolves division, selected and other-team colours consistently for labels and glyphs', () => {
    const owner = {...sampleScene.ships[0],id:'owner',relation:'self' as const,teamId:'a',divisionId:'17'};
    const mate = {...owner,id:'mate',relation:'ally' as const};
    const enemy = {...owner,id:'enemy',relation:'enemy' as const,teamId:'b'};
    const ships = [owner,mate,enemy];
    expect(shipIconColor(owner,ships,'owner','#4fe0a0')).toBe('#ff69b4');
    expect(shipIconColor(mate,ships,'owner','#4fe0a0')).toBe('#ffd369');
    expect(shipIconColor(enemy,ships,'owner','#f2665c')).toBe('#f2665c');
    expect(isOwnDivision(enemy,ships)).toBe(false);
  });
});
