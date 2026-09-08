import type { ReplayScene } from '../types';

export function BattleEnding({replay, elapsed}: {replay: ReplayScene['replay']; elapsed: number}) {
  if (elapsed < 0) return null;
  const outcome = replay.outcome;
  const title = outcome === 'victory' ? 'Victory' : outcome === 'defeat' ? 'Defeat'
    : outcome === 'draw' ? 'Draw' : replay.complete === false ? 'Recording ended' : 'Replay ended';
  const black = Math.max(0, Math.min(1, elapsed - 3));
  return <div className={`battle-ending ${outcome ?? 'unknown'}`} role="status" aria-label={title}>
    <div className="ending-fog"/>
    <strong className="ending-title">{title}</strong>
    <div className="ending-black" style={{opacity:black}}/>
  </div>;
}
