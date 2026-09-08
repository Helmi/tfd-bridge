import {it,expect} from 'vitest';
import {renderToStaticMarkup} from 'react-dom/server';
import {OwnerRibbons} from './OwnerStats';
import type {OwnerStats, RibbonDefinition} from '../types';

const realSample: OwnerStats = {damage:0,potentialDamage:0,spottingDamage:0,ribbons:{
  RIBBON_BULGE:2,RIBBON_CITADEL:1,RIBBON_MAIN_CALIBER_NO_PENETRATION:3,
  RIBBON_MAIN_CALIBER_OVER_PENETRATION:15,RIBBON_MAIN_CALIBER_PENETRATION:9,RIBBON_MAIN_CALIBER_RICOCHET:4,
}};
const mainDefinitions: Record<string,RibbonDefinition> = {
  RIBBON_MAIN_CALIBER:{label:'Main battery hits',iconKey:'ribbon_main_caliber',isSubribbon:false,imageUrl:'data:image/png;base64,original-main-ribbon'},
  RIBBON_CITADEL:{label:'Citadel hits',iconKey:'ribbon_citadel',isSubribbon:false},
};

it('collapses the recorded main-hit breakdown while keeping the standalone citadel separate',()=>{
  const html=renderToStaticMarkup(<OwnerRibbons stats={realSample} definitions={mainDefinitions}/>);
  expect(html).toContain('33 Main battery hits');
  expect(html).toContain('1 Citadel hits');
  expect(html).toContain('src="data:image/png;base64,original-main-ribbon"');
  expect(html).not.toContain('RIBBON_MAIN_CALIBER_PENETRATION');
  expect(html).not.toContain('RIBBON_BULGE');
});

it('preserves a recorded aggregate and excludes aircraft/secondary hits from a derived one',()=>{
  const mixed={...realSample,ribbons:{...realSample.ribbons,RIBBON_BOMB_PENETRATION:7,RIBBON_ROCKET_PENETRATION:6,RIBBON_SECONDARY_CALIBER:11}};
  expect(renderToStaticMarkup(<OwnerRibbons stats={mixed} definitions={mainDefinitions}/>)).toContain('33 Main battery hits');
  const explicit={...mixed,ribbons:{...mixed.ribbons,RIBBON_MAIN_CALIBER:80}};
  const html=renderToStaticMarkup(<OwnerRibbons stats={explicit} definitions={mainDefinitions}/>);
  expect(html).toContain('80 Main battery hits');
  expect(html).not.toContain('33 Main battery hits');
});

it('does not invent a main-hit ribbon before it is earned or from an aircraft citadel',()=>{
  const empty={...realSample,ribbons:{}};
  expect(renderToStaticMarkup(<OwnerRibbons stats={empty} definitions={mainDefinitions}/>)).not.toContain('Main battery hits');
  const aircraft={...empty,ribbons:{RIBBON_CITADEL:2,RIBBON_BOMB_PENETRATION:3}};
  const html=renderToStaticMarkup(<OwnerRibbons stats={aircraft} definitions={mainDefinitions}/>);
  expect(html).not.toContain('Main battery hits');
  expect(html).toContain('2 Citadel hits');
});

it('shows main ribbon totals without penetration and other hit sub-ribbons',()=>{
  const html=renderToStaticMarkup(<OwnerRibbons
    stats={{damage:0,potentialDamage:0,spottingDamage:0,ribbons:{hits:100,pens:60,overpens:30,ricochets:10,fires:2,kills:0}}}
    definitions={Object.fromEntries([
      ['hits','Main battery hits',false],['pens','Penetrations',true],
      ['overpens','Overpenetrations',true],['ricochets','Ricochets',true],
      ['fires','Set on fire',false],['kills','Destroyed',false],
    ].map(([id,label,sub])=>[String(id),{label:String(label),iconKey:String(id),isSubribbon:Boolean(sub)}]))}
  />);
  expect(html).toContain('100 Main battery hits');
  expect(html).toContain('2 Set on fire');
  expect(html).not.toContain('Penetrations');
  expect(html).not.toContain('Overpenetrations');
  expect(html).not.toContain('Ricochets');
  expect(html).not.toContain('Destroyed');
});
