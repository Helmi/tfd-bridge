import {renderToStaticMarkup} from 'react-dom/server';
import {describe,it,expect} from 'vitest';
import {BattleEnding} from './BattleEnding';
import type {ReplayScene} from '../types';
const replay=(outcome?:'victory'|'defeat'|'draw',complete=true)=>({outcome,complete}) as ReplayScene['replay'];
describe('shared battle ending',()=>{
  it.each(['victory','defeat','draw'] as const)('shows the recorded %s only at the end',outcome=>{
    expect(renderToStaticMarkup(<BattleEnding replay={replay(outcome)} elapsed={-0.01}/>)).toBe('');
    const html=renderToStaticMarkup(<BattleEnding replay={replay(outcome)} elapsed={0}/>);
    expect(html).toContain(`battle-ending ${outcome}`);
    expect(html).toContain('opacity:0');
  });
  it('holds for 3 seconds, fades for 1 second, then remains black',()=>{
    for(const [elapsed,opacity] of [[2.99,0],[3,0],[3.5,0.5],[4,1],[4.5,1]]) {
      expect(renderToStaticMarkup(<BattleEnding replay={replay('victory')} elapsed={elapsed}/>)).toContain(`opacity:${opacity}`);
    }
  });
  it('does not invent a victory or defeat for unknown/partial recordings',()=>{
    expect(renderToStaticMarkup(<BattleEnding replay={replay()} elapsed={0}/>)).toContain('Replay ended');
    expect(renderToStaticMarkup(<BattleEnding replay={replay(undefined,false)} elapsed={0}/>)).toContain('Recording ended');
  });
});
