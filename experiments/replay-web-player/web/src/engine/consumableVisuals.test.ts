import { describe, expect, it } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import { evaluateScene } from './timeline';
import { activeConsumableVisuals } from './consumableVisuals';
import { consumableIconKey, consumableIconUrl } from '../consumableIcons';
import type { ConsumableActivation, ReplayScene } from '../types';

const shipId = sampleScene.ships[0].id;
const hydro: ConsumableActivation = {id:'hydro',shipId,name:'HydroacousticSearch',start:10,end:120,
  visual:{source:'equipped-ability',iconKey:'PCY016_SonarSearchPremium',shipRadius:.14,torpedoRadius:.1,shipRangeMeters:5000,torpedoRangeMeters:3500}};
const radar: ConsumableActivation = {id:'radar',shipId,name:'Radar',start:20,end:47,
  visual:{source:'ship-definition',shipRadius:.25,shipRangeMeters:9000}};
const scene: ReplayScene = {...sampleScene, replay:{...sampleScene.replay,duration:200},
  ships:[{...sampleScene.ships[0],health:[{t:0,value:100}],knowledge:[{t:0,value:'spotted'}],
    pose:[{t:0,value:{x:.2,y:.3,yaw:0,course:0}},{t:100,value:{x:.6,y:.7,yaw:90,course:90}}]}],consumables:[hydro,radar]};

describe('observed active consumable visuals',()=>{
  it('uses reported intervals, simultaneous effects and backward seeks without retaining expired effects',()=>{
    for(const [t,names] of [[0,[]],[10,['HydroacousticSearch']],[20,['HydroacousticSearch','Radar']],
      [47,['HydroacousticSearch']],[120,[]],[30,['HydroacousticSearch','Radar']],[9,[]]] as const){
      expect(activeConsumableVisuals(evaluateScene(scene,t)).map(e=>e.definition.name)).toEqual(names);
    }
  });
  it('follows observed ship position and keeps hydro detection semantics separate',()=>{
    const a=activeConsumableVisuals(evaluateScene(scene,20))[0];
    const b=activeConsumableVisuals(evaluateScene(scene,40))[0];
    expect(b.ship.displayPose.x).toBeGreaterThan(a.ship.displayPose.x);
    expect([a.shipRadius,a.torpedoRadius]).toEqual([.14,.1]);
  });
  it.each(['hidden','last-known'] as const)('does not reveal %s ship effects',knowledge=>{
    const state=evaluateScene(scene,30);state.ships[0].knowledge=knowledge;
    expect(activeConsumableVisuals(state)).toEqual([]);
  });
  it('does not render a dead ship effect even if its reported duration continues',()=>{
    const state=evaluateScene(scene,30);state.ships[0].destroyed=true;
    expect(activeConsumableVisuals(state)).toEqual([]);
  });
  it('supports icon-only legacy/unknown-range observations and ignores unknown types',()=>{
    const legacy={...hydro,visual:undefined};
    const state=evaluateScene({...scene,consumables:[legacy,{...legacy,id:'unknown',name:'65'}]},30);
    expect(activeConsumableVisuals(state)).toHaveLength(1);
    expect(activeConsumableVisuals(state)[0].shipRadius).toBeUndefined();
    expect(consumableIconKey({...legacy,name:'65'})).toBeUndefined();
    expect(consumableIconUrl('../invalid')).toBeUndefined();
  });
  it('does not invent aircraft patrol circles even if stray range metadata exists',()=>{
    const state=evaluateScene({...scene,consumables:[{...hydro,name:'CatapultFighter'}]},30);
    const visual=activeConsumableVisuals(state)[0];
    expect(visual.shipRadius).toBeUndefined();expect(visual.torpedoRadius).toBeUndefined();
  });
  it('rejects nonfinite/negative radii and deduplicates repeated active notifications',()=>{
    const invalid={...hydro,visual:{...hydro.visual!,shipRadius:NaN,torpedoRadius:-1}};
    const v=activeConsumableVisuals(evaluateScene({...scene,consumables:[invalid,{...invalid,id:'duplicate'}]},30));
    expect(v).toHaveLength(1);expect(v[0].shipRadius).toBeUndefined();expect(v[0].torpedoRadius).toBeUndefined();
  });
});
