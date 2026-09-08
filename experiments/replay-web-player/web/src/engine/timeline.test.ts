import { describe, expect, it } from 'vitest';
import type { ReplayScene, ShipDefinition } from '../types';
import { evaluateScene, lerpAngle, samplePose, sampleStep } from './timeline';

const ship: ShipDefinition = {
  id: 'ship', teamId: 'blue', playerName: 'Tester', shipName: 'Test Ship', shipClass: 'cruiser', maxHealth: 10_000,
  pose: [
    { t: 0, value: { x: 0, y: 0, yaw: 350, course: 350, speed: 10 } },
    { t: 10, value: { x: 100, y: 50, yaw: 10, course: 20, speed: 20 } },
  ],
  health: [{ t: 0, value: 10_000 }, { t: 5, value: 6_000 }],
  knowledge: [{ t: 0, value: 'spotted' }, { t: 4, value: 'last-known' }, { t: 8, value: 'hidden' }],
};

const scene: ReplayScene = {
  schema: 'rocks.tfd.replay-scene/v0',
  replay: { id: 'test', title: 'Test', gameVersion: '0', duration: 10, perspectiveTeamId: 'blue', source: 'synthetic' },
  map: { name: 'Test', bounds: { minX: 0, minY: 0, maxX: 100, maxY: 100 } },
  teams: [{ id: 'blue', name: 'Blue', color: '#fff', score: [{ t: 0, value: 300 }] }],
  ships: [ship], captureZones: [], buffZones: [], damage: [],
  ordnance: [{
    id: 'shell', kind: 'shell', sourceId: 'ship', teamId: 'blue', start: 2, end: 4,
    trajectory: [{ t: 2, x: 0, y: 0 }, { t: 4, x: 20, y: 0 }],
  }],
};

describe('timeline evaluator', () => {
  it('follows each RPF sender, retains reports until updated, and rewinds without future bearings', () => {
    const rpfScene: ReplayScene = {...scene,
      ships:[{...ship,knowledge:[{t:0,value:'spotted'}]},
        {...ship,id:'other',playerName:'Other',knowledge:[{t:0,value:'spotted'}]}],
      chat:[
        {id:'new',t:7,senderId:'ship',senderName:'Tester',channel:'team',message:'RPF: NNW~N'},
        {id:'old',t:2,senderId:'ship',senderName:'Tester',channel:'team',message:'RPF: ESE~SE'},
        {id:'second',t:3,senderId:'other',senderName:'Other',channel:'team',message:'RPF: NW~NE'},
        {id:'normal',t:5,senderId:'ship',senderName:'Tester',channel:'team',message:'Thanks!'},
      ]};
    expect(evaluateScene(rpfScene,1).rpfSectors).toEqual([]);
    const initial = evaluateScene(rpfScene,2).rpfSectors;
    expect(initial).toEqual([{shipId:'ship',reportedAt:2,position:{x:20,y:10},startBearing:112.5,endBearing:135}]);
    const moved = evaluateScene(rpfScene,6).rpfSectors;
    expect(moved).toHaveLength(2);
    expect(moved[0]).toMatchObject({position:{x:60,y:30},startBearing:112.5,endBearing:135});
    expect(moved[1]).toMatchObject({shipId:'other',startBearing:315,endBearing:405});
    expect(evaluateScene(rpfScene,7).rpfSectors[0]).toMatchObject({reportedAt:7,startBearing:337.5,endBearing:360});
    expect(evaluateScene(rpfScene,2).rpfSectors).toEqual(initial);
    expect(evaluateScene(rpfScene,1).rpfSectors).toEqual([]);
  });

  it('anchors RPF to visible or last-known positions and hides it for hidden or sunk senders', () => {
    const rpfScene: ReplayScene = {...scene,chat:[
      {id:'rpf',t:1,senderId:ship.id,senderName:ship.playerName,channel:'team',message:'RPF: E~ESE'},
    ]};
    expect(evaluateScene(rpfScene,6).rpfSectors[0].position).toEqual({x:40,y:20});
    expect(evaluateScene(rpfScene,8).rpfSectors).toEqual([]);
    const sunk: ReplayScene = {...rpfScene,ships:[{...ship,health:[{t:0,value:100},{t:5,value:0}]}]};
    expect(evaluateScene(sunk,5).rpfSectors).toEqual([]);
  });

  it('uses a unique username for older chat records but never guesses ambiguous or invalid senders', () => {
    const rpfScene: ReplayScene = {...scene,chat:[
      {id:'rpf',t:1,senderName:ship.playerName,channel:'team',message:'RPF: E~ESE'},
    ]};
    expect(evaluateScene(rpfScene,2).rpfSectors[0].shipId).toBe(ship.id);
    expect(evaluateScene({...rpfScene,ships:[ship,{...ship,id:'duplicate'}]},2).rpfSectors).toEqual([]);
    expect(evaluateScene({...rpfScene,chat:[{...rpfScene.chat![0],senderId:'unknown'}]},2).rpfSectors).toEqual([]);
  });

  it('interpolates yaw over the shortest arc', () => {
    expect(lerpAngle(350, 10, 0.5)).toBe(0);
    expect(samplePose(ship.pose, 5)).toMatchObject({ x: 50, y: 25, yaw: 0, speed: 15 });
  });

  it('keeps discrete values stepped', () => {
    expect(sampleStep(ship.health, 4.99)).toBe(10_000);
    expect(sampleStep(ship.health, 5)).toBe(6_000);
  });

  it('freezes last-known display position without losing true semantic state', () => {
    const state = evaluateScene(scene, 6).ships[0];
    expect(state.pose.x).toBe(60);
    expect(state.displayPose.x).toBe(40);
    expect(state.knowledge).toBe('last-known');
  });

  it('only evaluates ordnance during its lifetime', () => {
    expect(evaluateScene(scene, 1).ordnance).toHaveLength(0);
    expect(evaluateScene(scene, 3).ordnance[0].position.x).toBe(10);
    expect(evaluateScene(scene, 5).ordnance).toHaveLength(0);
  });

  it('does not reveal an enemy before its first observation', () => {
    const enemy: ShipDefinition = {
      ...ship,
      id: 'enemy',
      relation: 'enemy',
      pose: ship.pose.map((sample) => ({ ...sample, t: sample.t + 5 })),
      knowledge: [{ t: 5, value: 'spotted' }],
    };
    const enemyScene = { ...scene, ships: [enemy] };
    expect(evaluateScene(enemyScene, 4).ships[0].knowledge).toBe('hidden');
    expect(evaluateScene(enemyScene, 5).ships[0].knowledge).toBe('spotted');
  });
});
