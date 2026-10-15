import { formatStat, ribbonList } from '../broadcast/intelModel';
import type { OwnerStats, RibbonDefinition } from '../types';

export function OwnerDamage({ stats }: { stats?: OwnerStats }) {
  return <div className="stats owner-damage">
    <div className="primary-stat"><strong>{formatStat(stats?.damage)}</strong><span>DAMAGE</span></div>
    <div><strong>{formatStat(stats?.potentialDamage)}</strong><span>POTENTIAL</span></div>
    <div><strong>{formatStat(stats?.spottingDamage)}</strong><span>SPOTTING</span></div>
  </div>;
}

export function OwnerRibbons({ stats, definitions = {} }: { stats?: OwnerStats; definitions?: Record<string, RibbonDefinition> }) {
  return <div className="ribbon-list" aria-label={stats ? 'Ribbons earned so far' : 'Ribbon timeline unavailable'}>
    {ribbonList(stats, definitions).map(({ key, label, count, imageUrl }) => <div className="ribbon" key={key} title={`${label}: ${count}`}>
      {imageUrl ? <img src={imageUrl} alt={label}/> : <span className="ribbon-fallback">{label}</span>}
      <strong aria-label={`${count} ${label}`}>{count}</strong>
    </div>)}
  </div>;
}
