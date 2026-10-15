import { endingModel } from '../engine/ending';
import type { ReplayScene } from '../types';

export function BattleEnding({replay, elapsed}: {replay: ReplayScene['replay']; elapsed: number}) {
  const model = endingModel(replay, elapsed);
  if (!model) return null;
  return <div className={`battle-ending ${replay.outcome ?? 'unknown'}`} role="status" aria-label={model.title}>
    <div className="ending-fog"/>
    <strong className="ending-title">{model.title}</strong>
    <div className="ending-black" style={{opacity:model.blackAlpha}}/>
  </div>;
}
