import {describe,it,expect} from 'vitest';
import {renderToStaticMarkup} from 'react-dom/server';
import {BroadcastFrame} from './BroadcastFrame';
import {sampleScene} from '../data/sampleScene';
import type {ReplayScene} from '../types';

const owner=sampleScene.ships[0];
const other=sampleScene.ships.find(s=>s.teamId!==owner.teamId)!;
const scene:ReplayScene={...sampleScene,replay:{...sampleScene.replay,perspectiveEntityId:owner.id},
  ownerSilhouette:'data:image/png;base64,owner-only',
  ownerStats:[{t:0,value:{damage:123456,potentialDamage:555555,spottingDamage:77777,ribbons:{hits:7,RIBBON_MAIN_CALIBER_PENETRATION:2,none:0}}}],
  ribbonDefinitions:{hits:{label:'Main battery hit',iconKey:'hit',isSubribbon:false,imageUrl:'data:image/png;base64,hit'},none:{label:'None',iconKey:'none',isSubribbon:false}},
  damage:[{t:5,sourceId:owner.id,targetId:other.id,amount:1200},{t:50,sourceId:owner.id,targetId:other.id,amount:900}] as ReplayScene['damage'],
  consumables:[{id:'c1',shipId:other.id,name:'RegenCrew',start:0,end:40}]};
const section=(html:string,tag:string)=>{
  const start=html.indexOf(`<section class="${tag}"`);
  return html.slice(start,html.indexOf('</section>',start)+'</section>'.length);
};
const render=(selectedShipId:string)=>renderToStaticMarkup(<BroadcastFrame scene={scene} time={30} selectedShipId={selectedShipId}/>);

describe('DOM markup of the selected-ship panel and the ribbons strip',()=>{
  it('owner player',()=>{ expect(section(render(owner.id),'player')).toBe('<section class="player" aria-label="Selected ship"><div class="identity"><span class="eyebrow">[TFD]</span><h1>Helmi</h1><div class="ship-title"><span class="ship-icon " role="img" title="Des Moines" aria-label="Des Moines, cruiser" style="mask-image:url(/assets/ship-classes/cruiser.svg);color:#ff69b4"></span><span>Des Moines</span></div></div><img class="silhouette" src="data:image/png;base64,owner-only" alt="Des Moines silhouette"/><div class="stats owner-damage"><div class="primary-stat"><strong>123,456</strong><span>DAMAGE</span></div><div><strong>555,555</strong><span>POTENTIAL</span></div><div><strong>77,777</strong><span>SPOTTING</span></div></div><div class="owner-hp"><span class="health-track"><i style="width:100%"></i></span><span><b>50,600</b> / 50,600 HP</span></div></section>'); });
  it('owner ribbons',()=>{ expect(section(render(owner.id),'ribbons')).toBe('<section class="ribbons" aria-label="Ribbons"><div class="ribbon-list" aria-label="Ribbons earned so far"><div class="ribbon" title="Main battery hit: 7"><img src="data:image/png;base64,hit" alt="Main battery hit"/><strong aria-label="7 Main battery hit">7</strong></div><div class="ribbon" title="Main battery hits: 2"><span class="ribbon-fallback">Main battery hits</span><strong aria-label="2 Main battery hits">2</strong></div></div></section>'); });
  it('other ship player',()=>{ expect(section(render(other.id),'player')).toBe('<section class="player" aria-label="Selected ship"><div class="identity"><span class="eyebrow">[KRAK]</span><h1>RedOctober</h1><div class="ship-title"><span class="ship-icon " role="img" title="Moskva" aria-label="Moskva, cruiser" style="mask-image:url(/assets/ship-classes/cruiser.svg);color:#ff69b4"></span><span>Moskva</span></div></div><div class="stats selected-metrics"><div><strong>173°</strong><span>HEADING</span></div><div><strong>Spotted</strong><span>VISIBILITY</span></div><div><strong>1,200</strong><span>OBSERVED HP LOST</span></div></div><div class="owner-hp"><span class="health-track"><i style="width:100%"></i></span><span><b>65,400</b> / 65,400 HP</span></div></section>'); });
  it('other ship ribbons',()=>{ expect(section(render(other.id),'ribbons')).toBe('<section class="ribbons" aria-label="Active consumables"><div class="selected-consumables"><span>Regen Crew <b>10s</b></span></div></section>'); });
});
