import { chatChannelColor } from '../chatColors';
import type { ReplayScene, ShipDefinition } from '../types';

export const ENTRANCE_SECONDS = 3.2;
export const ROW_GAP = 2;
// Seven minimum-height rows cover the 156px viewport, including a clipped oldest row.
export const VIEWPORT_ROWS = 7;

export type FeedEvent = {
  key: string; t: number; kind: 'kill' | 'chat';
  sender?: ShipDefinition; target?: ShipDefinition;
  senderName?: string; message?: string; color?: string;
};

export function buildFeedEvents(scene: ReplayScene): FeedEvent[] {
  const byId = new Map(scene.ships.map(ship => [ship.id, ship]));
  // Older exports can lack senderId. Only a unique exact player name is safe.
  const byName = new Map<string, ShipDefinition | undefined>();
  for (const ship of scene.ships) byName.set(ship.playerName, byName.has(ship.playerName) ? undefined : ship);
  const kills: FeedEvent[] = (scene.kills ?? []).map((e, i) => ({
    key: `kill-${i}`, t: e.t, kind: 'kill',
    sender: byId.get(e.killerId ?? ''), target: byId.get(e.victimId ?? ''),
  }));
  const chat: FeedEvent[] = (scene.chat ?? []).map((e, i) => ({
    key: `chat-${i}`, t: e.t, kind: 'chat',
    sender: byId.get(e.senderId ?? '') ?? byName.get(e.senderName),
    senderName: e.senderName, message: e.message, color: chatChannelColor(e.channel),
  }));
  return [...kills, ...chat].sort((a, b) => b.t - a.t);
}

export function feedEntranceRemaining(age: number): number {
  return (1 - Math.min(1, Math.max(0, age / ENTRANCE_SECONDS))) ** 3;
}
