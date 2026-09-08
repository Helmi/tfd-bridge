import {describe,it,expect} from 'vitest';
import {renderToStaticMarkup} from 'react-dom/server';
import {BroadcastFrame} from './BroadcastFrame';
import {sampleScene} from '../data/sampleScene';
import type {ReplayScene, ShipDefinition} from '../types';

const owner=sampleScene.ships[0];
const other=sampleScene.ships.find(s=>s.teamId!==owner.teamId)!;
const scene:ReplayScene={...sampleScene,replay:{...sampleScene.replay,perspectiveEntityId:owner.id},
  ownerSilhouette:'data:image/png;base64,owner-only',
  ownerStats:[{t:0,value:{damage:123456,potentialDamage:555555,spottingDamage:77777,ribbons:{hits:7}}}],
  ribbonDefinitions:{hits:{label:'Main battery hit',iconKey:'hit',isSubribbon:false}}};
const panel=(html:string)=>html.slice(html.indexOf('<aside class="intel">'),html.indexOf('<section class="feed"'));
describe('selected ship in the broadcast information panel',()=>{
  it('shows the selected ship and never substitutes owner-only data for it',()=>{
    const html=panel(renderToStaticMarkup(<BroadcastFrame scene={scene} time={30} selectedShipId={other.id}/>));
    expect(html).toContain(`<h1>${other.playerName}</h1>`);
    expect(html).toContain(other.shipName);
    expect(html).not.toContain(`<h1>${owner.playerName}</h1>`);
    expect(html).not.toContain('123,456');
    expect(html).not.toContain('owner-only');
    expect(html).not.toContain('Main battery hit');
    expect(html).toContain('VISIBILITY');
  });
  it('restores the owner counters, ribbons and silhouette on owner selection',()=>{
    const html=panel(renderToStaticMarkup(<BroadcastFrame scene={scene} time={30} selectedShipId={owner.id}/>));
    expect(html).toContain(`<h1>${owner.playerName}</h1>`);
    expect(html).toContain('123,456');
    expect(html).toContain('Main battery hit');
    expect(html).toContain('owner-only');
  });
});

it('mirrors the enemy score strip while keeping both rosters in class/tier/name order through selection and sinking', () => {
  const shuffled: Array<Pick<ShipDefinition, 'shipName' | 'shipClass' | 'tier'>> = [
    {shipName:'Zulu DD',shipClass:'destroyer',tier:10},
    {shipName:'Alpha BB',shipClass:'battleship',tier:8},
    {shipName:'Submarine',shipClass:'submarine',tier:10},
    {shipName:'Unknown BB',shipClass:'battleship'},
    {shipName:'Zulu BB',shipClass:'battleship',tier:10},
    {shipName:'Cruiser',shipClass:'cruiser',tier:9},
    {shipName:'Carrier',shipClass:'carrier',tier:8},
    {shipName:'Bravo BB',shipClass:'battleship',tier:10},
    {shipName:'Super BB',shipClass:'battleship',tier:11},
  ];
  const orderedScene: ReplayScene = {...scene, ships:['allies','enemies'].flatMap(teamId =>
    shuffled.map((ship, i) => ({...owner, ...ship, teamId, id:`${teamId}-${i}`,
      divisionLabel:undefined, playerName:`Player ${i}`, health:[{t:0,value:100},{t:20,value:0}],
    }))), replay:{...scene.replay,perspectiveEntityId:'allies-0'}};
  const expected = ['Carrier','Super BB','Bravo BB','Zulu BB','Alpha BB','Unknown BB','Cruiser','Zulu DD','Submarine'];
  const inputOrder = orderedScene.ships.map(s => s.id);
  for (const time of [0, 30]) for (const selectedShipId of ['allies-0', 'enemies-4']) {
    const html = renderToStaticMarkup(<BroadcastFrame scene={orderedScene} time={time} selectedShipId={selectedShipId}/>);
    const strips = [...html.matchAll(/<div class="survivors">(.*?)<\/div>/g)];
    const fleets = [...html.matchAll(/<section aria-label="(?:Allied|Enemy) fleet".*?<\/section>/g)];
    expect(strips).toHaveLength(2);
    expect(fleets).toHaveLength(2);
    const groups = [strips[0], strips[1], fleets[0], fleets[1]];
    for (const [index, group] of groups.entries()) {
      const icons = [...group[0].matchAll(/<span class="ship-icon[^"]*"[^>]*title="([^"]+)"/g)];
      expect(icons.map(icon => icon[1])).toEqual(index === 1 ? [...expected].reverse() : expected);
      expect(icons.every(icon => icon[0].includes('ship-icon dead'))).toBe(time === 30);
    }
  }
  expect(orderedScene.ships.map(s => s.id)).toEqual(inputOrder);
});
