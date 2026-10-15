import { describe, expect, it } from 'vitest';
import { sampleScene } from '../data/sampleScene';
import type { ReplayScene } from '../types';
import { buildFeedEvents, ENTRANCE_SECONDS, feedEntranceRemaining } from './feedModel';

const owner = { ...sampleScene.ships[0], id: 'owner', teamId: 'a', shipName: 'Owner ship', playerName: 'Owner', clan: 'HOME' };
const enemy = { ...sampleScene.ships[1], id: 'enemy', teamId: 'b', shipName: 'Enemy ship', playerName: 'Opponent', clan: 'AWAY' };
const base: ReplayScene = { ...sampleScene, ships: [owner, enemy], chat: [], kills: [] };

describe('buildFeedEvents', () => {
  it('resolves chat by ID first, with only a unique exact name fallback for older exports', () => {
    const scene: ReplayScene = { ...base, chat: [
      { id: '1', t: 1, senderId: enemy.id, senderName: owner.playerName, channel: 'global', message: 'ID wins' },
      { id: '2', t: 2, senderName: owner.playerName, channel: 'team', message: 'Legacy name' },
      { id: '3', t: 3, senderName: 'Absent', channel: 'system', message: 'No ship' },
    ] };
    const events = buildFeedEvents(scene);
    expect(events.map(e => e.sender?.id)).toEqual([undefined, owner.id, enemy.id]);
    expect(events[0].senderName).toBe('Absent');
    const duplicate = { ...scene, ships: [owner, enemy, { ...owner, id: 'duplicate' }] };
    expect(buildFeedEvents(duplicate)[1].sender).toBeUndefined();
  });

  it.each([
    ['team', '#4fe0a0'], ['division', '#ffd369'], ['system', '#7b9189'],
    ['global', '#eef4f1'], ['all', '#eef4f1'], ['unrecognized', '#eef4f1'],
  ])('gives %s chat the body colour %s, independent of sender affiliation', (channel, color) => {
    const [event] = buildFeedEvents({ ...base, chat: [{ id: '1', t: 1, senderId: enemy.id, senderName: enemy.playerName, channel, message: 'Test body' }] });
    expect(event).toMatchObject({ kind: 'chat', color, message: 'Test body', sender: { id: enemy.id } });
  });

  it('resolves kill killer and victim, and leaves missing participants undefined', () => {
    const events = buildFeedEvents({ ...base, kills: [{ t: 3, victimId: 'missing' }, { t: 2, killerId: 'missing', victimId: owner.id }, { t: 1, killerId: owner.id, victimId: enemy.id }] });
    expect(events.map(e => [e.sender?.id, e.target?.id])).toEqual([[undefined, undefined], [undefined, owner.id], [owner.id, enemy.id]]);
    expect(events.every(e => e.kind === 'kill')).toBe(true);
  });

  it('sorts newest first and merges kills with chat', () => {
    const events = buildFeedEvents({ ...base, kills: [{ t: 30, killerId: owner.id, victimId: enemy.id }], chat: [
      { id: 'later', t: 20, senderName: 'Later', channel: 'team', message: 'Second' },
      { id: 'earlier', t: 10, senderName: 'Earlier', channel: 'team', message: 'First' },
    ] });
    expect(events.map(e => e.key)).toEqual(['kill-0', 'chat-0', 'chat-1']);
  });
});

describe('feedEntranceRemaining', () => {
  it('uses replay-clock entrance progress for deterministic pause, seek and overlapping row motion', () => {
    expect(ENTRANCE_SECONDS).toBe(3.2);
    expect(feedEntranceRemaining(0)).toBe(1);
    expect(feedEntranceRemaining(1.6)).toBeCloseTo(0.125);
    expect(feedEntranceRemaining(3.2)).toBe(0);
    expect(feedEntranceRemaining(100)).toBe(0);
    expect(feedEntranceRemaining(-5)).toBe(1);
    // A wrapped 46px row plus a newer 26px row contribute independently.
    expect(46 * feedEntranceRemaining(1.6) + 26 * feedEntranceRemaining(0)).toBeCloseTo(31.75);
  });
});
