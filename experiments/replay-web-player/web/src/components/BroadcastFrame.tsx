import { useMemo } from 'react';
import { BattleEnding } from './BattleEnding';
import { TacticalMap, type TacticalMapCapture } from './TacticalMap';
import { OwnerDamage, OwnerRibbons } from './OwnerStats';
import { consumableList, formatNumber, selectedPanelModel } from '../broadcast/intelModel';
import { evaluateScene } from '../engine/timeline';
import { compareFleetShips } from '../engine/fleetOrder';
import { shipClassIconUrl } from '../shipClassIcons';
import { DIVISION_COLOR, isOwnDivision, shipIconColor } from '../divisions';
import { BattleFeed } from './BattleFeed';
import { buildScoreboardModel } from '../broadcast/scoreboardModel';
import type { EvaluatedShip, ReplayScene, ShipDefinition } from '../types';
import './broadcast.css';
import '../fonts.css';

type ClassIconProps = { name: string; shipClass: ShipDefinition['shipClass']; dead: boolean; color: string };
function ClassIcon({ name, shipClass, dead, color }: ClassIconProps) {
  return <span className={`ship-icon ${dead ? 'dead' : ''}`} role="img" title={name} aria-label={`${name}, ${shipClass}${dead ? ', sunk' : ''}`} style={{ maskImage: `url(${shipClassIconUrl(shipClass)})`, color }} />;
}
function ShipIcon({ ship, dead = false, ships = [], selectedId }: { ship: ShipDefinition; dead?: boolean; ships?: ShipDefinition[]; selectedId?: string }) {
  return <ClassIcon name={ship.shipName} shipClass={ship.shipClass} dead={dead} color={shipIconColor(ship, ships, selectedId, 'inherit')} />;
}

function Fleet({ ships, allShips, selectedId, onSelectShip, enemy = false }: { ships: EvaluatedShip[]; allShips: ShipDefinition[]; selectedId?: string; onSelectShip?: (id:string) => void; enemy?: boolean }) {
  return <section aria-label={enemy ? 'Enemy fleet' : 'Allied fleet'} className={`fleet ${enemy ? 'enemy' : 'friendly'}`}>
    {ships.map(s => <div className={`roster-row ${s.health <= 0 ? 'sunk' : ''} ${s.definition.id === selectedId ? 'self' : ''}`} key={s.definition.id} role={onSelectShip ? 'button' : undefined} tabIndex={onSelectShip ? 0 : undefined} onClick={() => onSelectShip?.(s.definition.id)} onKeyDown={e => {if(onSelectShip && (e.key === 'Enter' || e.key === ' ')){e.preventDefault();onSelectShip(s.definition.id);}}}>
      <ShipIcon ship={s.definition} ships={allShips} selectedId={selectedId} dead={s.health <= 0}/><div className="roster-name"><strong>{s.definition.divisionLabel && <span className="division-letter" style={{color:isOwnDivision(s.definition, allShips) ? DIVISION_COLOR : enemy ? 'var(--red)' : 'var(--green)'}} title={`Division ${s.definition.divisionLabel}`}>{s.definition.divisionLabel}</span>}{s.definition.shipName}</strong><span>{s.definition.clan ? `[${s.definition.clan}] ` : ''}{s.definition.playerName}</span></div>
      <span className="health-track" title={`${formatNumber(s.health)} / ${formatNumber(s.definition.maxHealth)}`}><i style={{width: `${Math.max(0, Math.min(100, s.health / s.definition.maxHealth * 100))}%`}}/></span>
    </div>)}
  </section>;
}

export function BroadcastFrame({ scene, time, selectedShipId, onSelectShip, silhouetteUrl, mapResolution, onRendererReady, onRendererError }: {
scene: ReplayScene; time: number; selectedShipId?: string; onSelectShip?: (id: string) => void; silhouetteUrl?: string;
mapResolution?: number;
onRendererReady?: (renderer: TacticalMapCapture) => void; onRendererError?: (error: Error) => void;
}) {
  const endingElapsed = time - scene.replay.duration;
  time = Math.min(time, scene.replay.duration);
  const state = useMemo(() => evaluateScene(scene, time), [scene, time]);
  const owner = state.ships.find(s => s.definition.id === scene.replay.perspectiveEntityId) ?? state.ships.find(s => s.definition.relation === 'self') ?? state.ships[0];
  const selectedId = selectedShipId ?? owner.definition.id;
  const panel = selectedPanelModel({ scene, time, state, selectedId, ownerId: owner.definition.id, ownerTeamId: owner.definition.teamId });
  const { isOwner } = panel;
  const selectedConsumables = consumableList(state.consumables, panel.definition.id);
  silhouetteUrl ??= scene.ownerSilhouette;
  const fleetOrder = (a: EvaluatedShip, b: EvaluatedShip) => compareFleetShips(a.definition, b.definition);
  const ally = state.ships.filter(s => s.definition.teamId === owner.definition.teamId).sort(fleetOrder);
  const enemy = state.ships.filter(s => s.definition.teamId !== owner.definition.teamId).sort(fleetOrder);
  const scoreboard = buildScoreboardModel({ scene, time, state, selectedId, ownerTeamId: owner.definition.teamId, ally, enemy }, { friendly: 'inherit', enemy: 'inherit' });
  return <div className="broadcast">
      <section className="map-panel"><TacticalMap resolution={mapResolution} presentation="broadcast" scene={scene} time={time} selectedShipId={selectedId} onSelectShip={onSelectShip ?? (() => {})} onRendererReady={onRendererReady} onRendererError={onRendererError}/>
        <div className="scoreboard">
          <div className="team-score friendly"><div className="score-number">{scoreboard.friendlyScore}</div><div className="survivors">{scoreboard.friendlyShips.map(s => <ClassIcon key={s.id} name={s.shipName} shipClass={s.shipClass} dead={s.dead} color={s.color}/>)}</div></div>
          <div className="battle-clock"><span>{scoreboard.mapLabel}</span><strong>{scoreboard.clock}</strong><div className="caps">{scoreboard.caps.map(c => <span key={c.id} className={c.owner ?? ''}>{c.label}</span>)}</div></div>
          <div className="team-score enemy"><div className="score-number">{scoreboard.enemyScore}</div><div className="survivors">{scoreboard.enemyShips.map(s => <ClassIcon key={s.id} name={s.shipName} shipClass={s.shipClass} dead={s.dead} color={s.color}/>)}</div></div>
        </div>
      </section>
      <aside className="intel">
        <section className="player" aria-label="Selected ship"><div className="identity"><span className="eyebrow">{panel.eyebrow}</span><h1>{panel.name}</h1><div className="ship-title"><ShipIcon ship={panel.definition} ships={scene.ships} selectedId={selectedId}/><span>{panel.shipName}</span></div></div>{isOwner && silhouetteUrl && <img className="silhouette" src={silhouetteUrl} alt={`${owner.definition.shipName} silhouette`}/>}
          {isOwner ? <OwnerDamage stats={state.ownerStats}/> : <div className="stats selected-metrics">
            {panel.stats.map(stat => <div key={stat.label}><strong>{stat.value}</strong><span>{stat.label}</span></div>)}
          </div>}
          <div className="owner-hp"><span className="health-track"><i style={{width:`${panel.healthRatio * 100}%`}}/></span><span><b>{formatNumber(panel.health)}</b> / {formatNumber(panel.maxHealth)} HP</span></div>
        </section>
        <section className="ribbons" aria-label={isOwner ? 'Ribbons' : 'Active consumables'}>{isOwner ? <OwnerRibbons stats={state.ownerStats} definitions={scene.ribbonDefinitions}/> : <div className="selected-consumables">{selectedConsumables.length ? selectedConsumables.map((c,i)=><span key={i}>{c.label} <b>{c.seconds}s</b></span>) : <span className="muted">No active consumables observed</span>}</div>}</section>
        <BattleFeed scene={scene} time={time} ownerTeamId={owner.definition.teamId}/>
        <div className="fleets"><Fleet ships={ally} allShips={scene.ships} selectedId={selectedId} onSelectShip={onSelectShip}/><Fleet ships={enemy} allShips={scene.ships} selectedId={selectedId} onSelectShip={onSelectShip} enemy/></div>
      </aside>
      <BattleEnding replay={scene.replay} elapsed={endingElapsed}/>
  </div>;
}
