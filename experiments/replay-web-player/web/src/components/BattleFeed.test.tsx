import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { BattleFeed, buildFeedEvents, feedEntranceRemaining } from './BattleFeed';
import { sampleScene } from '../data/sampleScene';
import type { ReplayScene } from '../types';

const owner = {...sampleScene.ships[0], id:'owner', teamId:'a', shipName:'Owner ship', playerName:'Owner', clan:'HOME'};
const enemy = {...sampleScene.ships[1], id:'enemy', teamId:'b', shipName:'Enemy ship', playerName:'Opponent', clan:'AWAY'};
const base: ReplayScene = {...sampleScene, ships:[owner, enemy], chat:[], kills:[]};
const markup = (scene: ReplayScene, time = 100) => renderToStaticMarkup(<BattleFeed scene={scene} time={time} ownerTeamId="a"/>);

describe('battle feed identity and channel coverage', () => {
  it('resolves chat by ID first, with only unique exact name fallback for older exports', () => {
    const scene: ReplayScene = {...base, chat:[
      {id:'1',t:1,senderId:enemy.id,senderName:owner.playerName,channel:'global',message:'ID wins'},
      {id:'2',t:2,senderName:owner.playerName,channel:'team',message:'Legacy name'},
      {id:'3',t:3,senderName:'Absent',channel:'system',message:'No ship'},
    ]};
    const events = buildFeedEvents(scene);
    expect(events.map(e => e.sender?.id)).toEqual([undefined, owner.id, enemy.id]);
    const duplicate = {...scene, ships:[owner, enemy, {...owner,id:'duplicate'}]};
    expect(buildFeedEvents(duplicate)[1].sender).toBeUndefined();
    const html = markup(scene);
    expect(html).toContain('Owner ship');
    expect(html).toContain('| [HOME] Owner');
    expect(html).toContain('Enemy ship');
    expect(html).toContain('| [AWAY] Opponent');
    expect(html).toContain('<span class="player-handle">Absent</span>');
  });

  it.each([
    ['team','#4fe0a0'],['division','#ffd369'],['system','#7b9189'],
    ['global','#eef4f1'],['all','#eef4f1'],['unrecognized','#eef4f1'],
  ])('keeps %s body colour independent of sender affiliation', (channel, color) => {
    const html = markup({...base,chat:[{id:'1',t:1,senderId:enemy.id,senderName:enemy.playerName,channel,message:'Test body'}]});
    expect(html).toContain('participant enemy');
    expect(html).toContain(`class="chat-message" style="color:${color}">Test body`);
    expect(html).toContain('feed-class-icon');
    expect(html).toContain('m9 18 6-6-6-6');
  });

  it('shows both ship/player identities for kills, but does not invent missing participants or classes', () => {
    const scene: ReplayScene = {...base,kills:[{t:1,killerId:owner.id,victimId:enemy.id},{t:2,killerId:'missing',victimId:owner.id},{t:3,victimId:'missing'}]};
    const html = markup(scene);
    expect(html.match(/ sunk /g)).toHaveLength(3);
    expect(html).not.toContain('destroyed');
    expect(html.match(/participant unknown/g)).toHaveLength(3);
    expect(html.match(/feed-class-icon/g)).toHaveLength(3);
    expect(html).toContain('M18 6 6 18');
    const invalid = {...base,ships:[{...owner,shipClass:'unknown' as typeof owner.shipClass}],kills:[{t:1,killerId:owner.id,victimId:'missing'}]};
    expect(markup(invalid)).not.toContain('feed-class-icon');
    expect(markup(invalid)).not.toContain('undefined.svg');
  });

  it('keeps chronology, hides future events and restores empty state on backwards seeking', () => {
    const scene: ReplayScene = {...base,kills:[{t:30,killerId:owner.id,victimId:enemy.id}],chat:[
      {id:'later',t:20,senderName:'Later',channel:'team',message:'Second'},
      {id:'earlier',t:10,senderName:'Earlier',channel:'team',message:'First'},
    ]};
    const html = markup(scene,25);
    expect(html.indexOf('First')).toBeLessThan(html.indexOf('Second'));
    expect(html).not.toContain(' sunk ');
    expect(markup(scene,0)).toContain('Waiting for the first battle event');
    expect(markup(scene,0)).not.toContain('First');
  });
});

it('uses replay-clock entrance progress for deterministic pause, seek and overlapping row motion', () => {
  expect(feedEntranceRemaining(0)).toBe(1);
  expect(feedEntranceRemaining(1.6)).toBeCloseTo(.125);
  expect(feedEntranceRemaining(3.2)).toBe(0);
  expect(feedEntranceRemaining(100)).toBe(0);
  // A wrapped 46px row plus a newer 26px row contribute independently.
  expect(46 * feedEntranceRemaining(1.6) + 26 * feedEntranceRemaining(0)).toBeCloseTo(31.75);
});
