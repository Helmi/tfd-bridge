import type { OwnerStats, RibbonDefinition } from '../types';

const format = (value: number | undefined) => value === undefined ? '—' : Math.round(value).toLocaleString('en-US');

// Canonical wows-toolkit Ribbon translation keys for the main hit breakdown.
// Citadel is a separate main ribbon (and can be earned by AP aircraft), so it
// remains separate instead of being attributed to main guns a second time.
const MAIN_HIT_SUBRIBBONS = [
  'RIBBON_MAIN_CALIBER_PENETRATION',
  'RIBBON_MAIN_CALIBER_OVER_PENETRATION',
  'RIBBON_MAIN_CALIBER_NO_PENETRATION',
  'RIBBON_MAIN_CALIBER_RICOCHET',
  'RIBBON_BULGE',
];
const MAIN_HITS = 'RIBBON_MAIN_CALIBER';

export function OwnerDamage({ stats }: { stats?: OwnerStats }) {
  return <div className="stats owner-damage">
    <div className="primary-stat"><strong>{format(stats?.damage)}</strong><span>DAMAGE</span></div>
    <div><strong>{format(stats?.potentialDamage)}</strong><span>POTENTIAL</span></div>
    <div><strong>{format(stats?.spottingDamage)}</strong><span>SPOTTING</span></div>
  </div>;
}

export function OwnerRibbons({ stats, definitions = {} }: { stats?: OwnerStats; definitions?: Record<string, RibbonDefinition> }) {
  const counts = {...stats?.ribbons};
  // Modern live updates may only carry the breakdown. Collapse it for this
  // display, preserving any explicitly recorded aggregate without adding twice.
  if (counts[MAIN_HITS] === undefined) {
    const total = MAIN_HIT_SUBRIBBONS.reduce((sum, key) => sum + (counts[key] ?? 0), 0);
    if (total > 0) counts[MAIN_HITS] = total;
  }
  const ribbons = Object.entries(counts).filter(([key, count]) => count > 0 && !definitions[key]?.isSubribbon && !MAIN_HIT_SUBRIBBONS.includes(key)).sort(([a], [b]) => a.localeCompare(b));
  return <div className="ribbon-list" aria-label={stats ? 'Ribbons earned so far' : 'Ribbon timeline unavailable'}>
    {ribbons.map(([key, count]) => {
      const definition = definitions[key];
      const label = definition?.label ?? (key === MAIN_HITS ? 'Main battery hits' : key);
      return <div className="ribbon" key={key} title={`${label}: ${count}`}>
        {definition?.imageUrl ? <img src={definition.imageUrl} alt={label}/> : <span className="ribbon-fallback">{label}</span>}
        <strong aria-label={`${count} ${label}`}>{count}</strong>
      </div>;
    })}
  </div>;
}
