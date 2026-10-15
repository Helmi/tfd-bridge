import type { EvaluatedConsumable, OwnerStats, RibbonDefinition, ShipDefinition } from '../types';
import type { BroadcastFrameState } from './types';

export const formatStat = (value: number | undefined) => value === undefined ? '—' : Math.round(value).toLocaleString('en-US');
export const formatNumber = (n: number) => Math.round(n).toLocaleString('en-US');

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

export interface PanelStat { label: string; value: string; primary?: boolean }

export interface SelectedPanelModel {
  eyebrow: string;
  name: string;
  shipName: string;
  definition: ShipDefinition;
  isOwner: boolean;
  /** DAMAGE, POTENTIAL, SPOTTING for the owner. HEADING, VISIBILITY, OBSERVED HP LOST otherwise. */
  stats: PanelStat[];
  health: number;
  maxHealth: number;
  /** `health / maxHealth` clamped to 0..1. */
  healthRatio: number;
}

type PanelFrame = Pick<BroadcastFrameState, 'scene' | 'time' | 'state' | 'selectedId' | 'ownerId' | 'ownerTeamId'>;

export function selectedPanelModel({ scene, time, state, selectedId, ownerId, ownerTeamId }: PanelFrame): SelectedPanelModel {
  const owner = state.ships.find(s => s.definition.id === ownerId) ?? state.ships[0];
  const selected = state.ships.find(s => s.definition.id === selectedId) ?? owner;
  const { definition } = selected;
  const isOwner = definition.id === owner.definition.id;
  const at = Math.min(time, scene.replay.duration);
  const stats: PanelStat[] = isOwner
    ? [
      { label: 'DAMAGE', value: formatStat(state.ownerStats?.damage), primary: true },
      { label: 'POTENTIAL', value: formatStat(state.ownerStats?.potentialDamage) },
      { label: 'SPOTTING', value: formatStat(state.ownerStats?.spottingDamage) },
    ]
    : [
      { label: 'HEADING', value: `${Math.round(selected.pose.yaw).toString().padStart(3, '0')}°` },
      { label: 'VISIBILITY', value: selected.health <= 0 ? 'Sunk' : { spotted: 'Spotted', 'last-known': 'Last known', hidden: 'Unspotted' }[selected.knowledge] },
      { label: 'OBSERVED HP LOST', value: formatNumber(scene.damage.filter(e => e.targetId === definition.id && e.t <= at).reduce((total, e) => total + e.amount, 0)) },
    ];
  return {
    eyebrow: definition.clan ? `[${definition.clan}]` : isOwner ? 'REPLAY OWNER' : definition.teamId === ownerTeamId ? 'ALLY' : 'ENEMY',
    name: definition.playerName,
    shipName: definition.shipName,
    definition,
    isOwner,
    stats,
    health: selected.health,
    maxHealth: definition.maxHealth,
    healthRatio: Math.max(0, Math.min(1, selected.health / definition.maxHealth)),
  };
}

export interface RibbonItem { key: string; label: string; count: number; imageUrl?: string }

export function ribbonList(stats?: OwnerStats, definitions: Record<string, RibbonDefinition> = {}): RibbonItem[] {
  const counts = {...stats?.ribbons};
  // Modern live updates may only carry the breakdown. Collapse it for this
  // display, preserving any explicitly recorded aggregate without adding twice.
  if (counts[MAIN_HITS] === undefined) {
    const total = MAIN_HIT_SUBRIBBONS.reduce((sum, key) => sum + (counts[key] ?? 0), 0);
    if (total > 0) counts[MAIN_HITS] = total;
  }
  return Object.entries(counts)
    .filter(([key, count]) => count > 0 && !definitions[key]?.isSubribbon && !MAIN_HIT_SUBRIBBONS.includes(key))
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, count]) => {
      const definition = definitions[key];
      return { key, label: definition?.label ?? (key === MAIN_HITS ? 'Main battery hits' : key), count, imageUrl: definition?.imageUrl };
    });
}

export interface ConsumableItem { label: string; seconds: number }

export function consumableList(consumables: EvaluatedConsumable[], shipId: string): ConsumableItem[] {
  return consumables
    .filter(c => c.definition.shipId === shipId)
    .map(c => ({ label: c.definition.name.replace(/([a-z\d])([A-Z])/g, '$1 $2'), seconds: Math.ceil(c.remaining) }));
}
