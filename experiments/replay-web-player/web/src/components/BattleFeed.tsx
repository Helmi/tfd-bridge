import { useLayoutEffect, useMemo, useRef } from 'react';
import { chatChannelColor } from '../chatColors';
import { shipClassIconUrl } from '../shipClassIcons';
import type { ReplayScene, ShipDefinition } from '../types';

const clock = (t: number) => `${Math.floor(t / 60).toString().padStart(2, '0')}:${Math.floor(t % 60).toString().padStart(2, '0')}`;
const ENTRANCE_SECONDS = 3.2;
const ROW_GAP = 2;
// Seven minimum-height rows cover the 156px viewport, including a clipped oldest row.
const VIEWPORT_ROWS = 7;

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

function Participant({ ship, ownerTeamId, fallback }: { ship?: ShipDefinition; ownerTeamId: string; fallback?: string }) {
  if (!ship) return fallback
    ? <span className="player-handle">{fallback}</span>
    : <span className="participant unknown"><b>Unknown</b></span>;
  // Runtime guard: old/custom scene files may contain an unrecognised class.
  const knownClass = ['carrier', 'battleship', 'cruiser', 'destroyer', 'submarine'].includes(ship.shipClass);
  return <span className={`participant ${ship.teamId === ownerTeamId ? 'friendly' : 'enemy'}`}>
    <span className="ship-label">{knownClass && <span className="feed-class-icon" role="img" aria-label={ship.shipClass}
      style={{ maskImage: `url(${shipClassIconUrl(ship.shipClass)})` }}/>}<b>{ship.shipName}</b></span>{' '}
    {ship.playerName && <span className="player-handle">| {ship.clan ? `[${ship.clan}] ` : ''}{ship.playerName}</span>}
  </span>;
}

// Official Lucide ChevronRight and X paths. See public/assets/lucide/LICENSE.
function EventIcon({ kind }: { kind: FeedEvent['kind'] }) {
  return <span className="event-symbol" aria-hidden="true"><svg width="24" height="24" viewBox="0 0 24 24"
    fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    {kind === 'chat' ? <path d="m9 18 6-6-6-6"/> : <><path d="M18 6 6 18"/><path d="m6 6 12 12"/></>}
  </svg></span>;
}

export function BattleFeed({ scene, time, ownerTeamId }: { scene: ReplayScene; time: number; ownerTeamId: string }) {
  const events = useMemo(() => buildFeedEvents(scene), [scene]);
  const arrived = events.filter(e => e.t <= time);
  const entering = arrived.filter(e => time - e.t < ENTRANCE_SECONDS).length;
  const visible = arrived.slice(0, VIEWPORT_ROWS + entering).reverse();
  const visibleKey = visible.map(e => e.key).join(',');
  const stack = useRef<HTMLDivElement>(null);
  const heights = useRef(new Map<string, number>());

  useLayoutEffect(() => {
    const element = stack.current!;
    const measure = () => {
      heights.current.clear();
      for (const row of element.querySelectorAll<HTMLElement>('[data-event-key]')) {
        // offsetHeight is unscaled: interactive viewports scale the entire frame.
        heights.current.set(row.dataset.eventKey!, row.offsetHeight + ROW_GAP);
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [visibleKey, scene]);

  useLayoutEffect(() => {
    // Sum actual row heights so wrapping and simultaneous arrivals move the
    // whole stack continuously. Runs inside flushSync for offline capture too.
    const offset = visible.reduce((sum, e) => sum + (heights.current.get(e.key) ?? 26) * feedEntranceRemaining(time - e.t), 0);
    stack.current!.style.transform = `translateY(${offset}px)`;
  });

  return <section className="feed" aria-label="Battle events and chat"><div className="feed-window">
    <div className={`feed-lines${visible.length ? '' : ' is-empty'}`} ref={stack}>
      {visible.map(e => <div className={`feed-row ${e.kind}`} data-event-key={e.key} key={e.key}>
        <time>{clock(e.t)}</time><EventIcon kind={e.kind}/><div className="feed-content">
          <Participant ship={e.sender} ownerTeamId={ownerTeamId} fallback={e.kind === 'chat' ? e.senderName || 'Unknown' : undefined}/>
          {e.kind === 'chat' ? <span className="chat-message" style={{color:e.color}}>{e.message}</span>
            : <><span className="event-verb"> sunk </span><Participant ship={e.target} ownerTeamId={ownerTeamId}/></>}
        </div>
      </div>)}
      {!visible.length && <p className="empty">Waiting for the first battle event…</p>}
    </div>
  </div></section>;
}
