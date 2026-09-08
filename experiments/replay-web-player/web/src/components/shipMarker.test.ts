import { describe, expect, it } from 'vitest';
import { Texture, TextureSource } from 'pixi.js';
import { createShipMarker } from './shipMarker';
import { isOwnDivision } from '../divisions';
import { sampleScene } from '../data/sampleScene';
import type { ShipClass } from '../shipClassIcons';

const classes: ShipClass[] = ['carrier','battleship','cruiser','destroyer','submarine'];
describe('ship glyph remains undecorated', () => {
  it.each(classes)('%s has only its glyph at every heading and visibility state', shipClass => {
    const texture = new Texture({source:new TextureSource({width:72,height:128})});
    for (const yaw of [0,90,180,270,137.8125,296.71875]) for (const alpha of [1,.42,.3]) {
      const marker = createShipMarker({texture,shipClass,position:{x:143,y:257},yaw,scale:1.3,
        color:'#ff69b4',alpha,selected:true});
      expect(marker.children.map(c => c.label)).toEqual(['ship-glyph']);
      expect(marker.alpha).toBe(alpha);
      expect(marker.rotation).toBeCloseTo(yaw*Math.PI/180);
      expect(marker.children[0].tint).toBe(0xff69b4);
      marker.destroy({children:true});
    }
    texture.destroy(true);
  });
  it('keeps unknown/missing texture fallback undecorated too', () => {
    const marker = createShipMarker({shipClass:'cruiser',position:{x:0,y:0},yaw:0,scale:1,
      color:'#ffd369',alpha:.3,selected:false});
    expect(marker.children.map(c => c.label)).toEqual(['ship-glyph']);
    marker.destroy({children:true});
  });
});

it('includes the recorder in a valid same-team division, excluding other divisions and solo players', () => {
  const owner = {...sampleScene.ships[0],id:'owner',relation:'self' as const,teamId:'a',divisionId:'17'};
  const ships = [owner,
    {...owner,id:'mate',relation:'ally' as const},
    {...owner,id:'other',relation:'ally' as const,divisionId:'18'},
    {...owner,id:'enemy',relation:'enemy' as const,teamId:'b'},
    {...owner,id:'solo',relation:'ally' as const,divisionId:'0'},
  ];
  expect(ships.filter(ship => isOwnDivision(ship,ships)).map(ship => ship.id)).toEqual(['owner','mate']);
  for (const divisionId of [undefined, '0']) {
    const solo = {...owner,divisionId};
    expect(isOwnDivision(solo,[solo])).toBe(false);
  }
  const marker = createShipMarker({shipClass:'cruiser',position:{x:0,y:0},yaw:0,scale:1,color:'#ff69b4',alpha:1,selected:true});
  expect(marker.getChildByLabel('division-name-dot')).toBeNull();
  marker.destroy({children:true});
});
