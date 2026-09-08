import { useMemo } from 'react';
import { BattleEnding } from './BattleEnding';
import { TacticalMap, type TacticalMapCapture } from './TacticalMap';
import { OwnerDamage, OwnerRibbons } from './OwnerStats';
import { evaluateScene } from '../engine/timeline';
import { compareFleetShips } from '../engine/fleetOrder';
import { shipClassIconUrl } from '../shipClassIcons';
import { DIVISION_COLOR, isOwnDivision, shipIconColor } from '../divisions';
import { BattleFeed } from './BattleFeed';
import type { EvaluatedShip, ReplayScene, ShipDefinition } from '../types';
import './broadcast.css';
import '../fonts.css';

const clock = (t: number) => `${Math.floor(t / 60).toString().padStart(2, '0')}:${Math.floor(t % 60).toString().padStart(2, '0')}`;
const number = (n: number) => Math.round(n).toLocaleString('en-US');
function ShipIcon({ ship, dead = false, ships = [], selectedId }: { ship: ShipDefinition; dead?: boolean; ships?: ShipDefinition[]; selectedId?: string }) {
  return <span className={`ship-icon ${dead ? 'dead' : ''}`} role="img" title={ship.shipName} aria-label={`${ship.shipName}, ${ship.shipClass}${dead ? ', sunk' : ''}`} style={{ maskImage: `url(${shipClassIconUrl(ship.shipClass)})`, color: shipIconColor(ship, ships, selectedId, 'inherit') }} />;
}

function Fleet({ ships, allShips, selectedId, onSelectShip, enemy = false }: { ships: EvaluatedShip[]; allShips: ShipDefinition[]; selectedId?: string; onSelectShip?: (id:string) => void; enemy?: boolean }) {
  return <section aria-label={enemy ? 'Enemy fleet' : 'Allied fleet'} className={`fleet ${enemy ? 'enemy' : 'friendly'}`}>
    {ships.map(s => <div className={`roster-row ${s.health <= 0 ? 'sunk' : ''} ${s.definition.id === selectedId ? 'self' : ''}`} key={s.definition.id} role={onSelectShip ? 'button' : undefined} tabIndex={onSelectShip ? 0 : undefined} onClick={() => onSelectShip?.(s.definition.id)} onKeyDown={e => {if(onSelectShip && (e.key === 'Enter' || e.key === ' ')){e.preventDefault();onSelectShip(s.definition.id);}}}>
      <ShipIcon ship={s.definition} ships={allShips} selectedId={selectedId} dead={s.health <= 0}/><div className="roster-name"><strong>{s.definition.divisionLabel && <span className="division-letter" style={{color:isOwnDivision(s.definition, allShips) ? DIVISION_COLOR : enemy ? 'var(--red)' : 'var(--green)'}} title={`Division ${s.definition.divisionLabel}`}>{s.definition.divisionLabel}</span>}{s.definition.shipName}</strong><span>{s.definition.clan ? `[${s.definition.clan}] ` : ''}{s.definition.playerName}</span></div>
      <span className="health-track" title={`${number(s.health)} / ${number(s.definition.maxHealth)}`}><i style={{width: `${Math.max(0, Math.min(100, s.health / s.definition.maxHealth * 100))}%`}}/></span>
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
  const selected = state.ships.find(s => s.definition.id === selectedId) ?? owner;
  const isOwner = selected.definition.id === owner.definition.id;
  const selectedConsumables = state.consumables.filter(c => c.definition.shipId === selected.definition.id);
  const damageTaken = scene.damage.filter(e => e.targetId === selected.definition.id && e.t <= time).reduce((total,e) => total+e.amount,0);
  const visibility = selected.health <= 0 ? 'Sunk' : ({spotted:'Spotted','last-known':'Last known',hidden:'Unspotted'}[selected.knowledge]);
  silhouetteUrl ??= scene.ownerSilhouette;
  const fleetOrder = (a: EvaluatedShip, b: EvaluatedShip) => compareFleetShips(a.definition, b.definition);
  const ally = state.ships.filter(s => s.definition.teamId === owner.definition.teamId).sort(fleetOrder);
  const enemy = state.ships.filter(s => s.definition.teamId !== owner.definition.teamId).sort(fleetOrder);
  return <div className="broadcast">
      <section className="map-panel"><TacticalMap resolution={mapResolution} presentation="broadcast" scene={scene} time={time} selectedShipId={selectedId} onSelectShip={onSelectShip ?? (() => {})} onRendererReady={onRendererReady} onRendererError={onRendererError}/>
        <div className="scoreboard">
          <div className="team-score friendly"><div className="score-number">{state.scores[owner.definition.teamId] ?? 0}</div><div className="survivors">{ally.map(s => <ShipIcon key={s.definition.id} ship={s.definition} ships={scene.ships} selectedId={selectedId} dead={s.health <= 0}/>)}</div></div>
          <div className="battle-clock"><span>{scene.map.name.split('/').pop()?.replace(/^\d+_(?:NE_)?/, '').replace(/_/g, ' ').toUpperCase()}</span><strong>{clock(time)}</strong><div className="caps">{state.captureZones.map(c => <span key={c.definition.id} className={c.owner === owner.definition.teamId ? 'friendly' : c.owner ? 'enemy' : ''}>{c.definition.label}</span>)}</div></div>
          <div className="team-score enemy"><div className="score-number">{state.scores[enemy[0]?.definition.teamId] ?? 0}</div><div className="survivors">{[...enemy].reverse().map(s => <ShipIcon key={s.definition.id} ship={s.definition} ships={scene.ships} selectedId={selectedId} dead={s.health <= 0}/>)}</div></div>
        </div>
      </section>
      <aside className="intel">
        <section className="player" aria-label="Selected ship"><div className="identity"><span className="eyebrow">{selected.definition.clan ? `[${selected.definition.clan}]` : isOwner ? 'REPLAY OWNER' : selected.definition.teamId === owner.definition.teamId ? 'ALLY' : 'ENEMY'}</span><h1>{selected.definition.playerName}</h1><div className="ship-title"><ShipIcon ship={selected.definition} ships={scene.ships} selectedId={selectedId}/><span>{selected.definition.shipName}</span></div></div>{isOwner && silhouetteUrl && <img className="silhouette" src={silhouetteUrl} alt={`${owner.definition.shipName} silhouette`}/>}
          {isOwner ? <OwnerDamage stats={state.ownerStats}/> : <div className="stats selected-metrics">
            <div><strong>{Math.round(selected.pose.yaw).toString().padStart(3,'0')}°</strong><span>HEADING</span></div>
            <div><strong>{visibility}</strong><span>VISIBILITY</span></div>
            <div><strong>{number(damageTaken)}</strong><span>OBSERVED HP LOST</span></div>
          </div>}
          <div className="owner-hp"><span className="health-track"><i style={{width:`${Math.max(0,Math.min(100,selected.health / selected.definition.maxHealth * 100))}%`}}/></span><span><b>{number(selected.health)}</b> / {number(selected.definition.maxHealth)} HP</span></div>
        </section>
        <section className="ribbons" aria-label={isOwner ? 'Ribbons' : 'Active consumables'}>{isOwner ? <OwnerRibbons stats={state.ownerStats} definitions={scene.ribbonDefinitions}/> : <div className="selected-consumables">{selectedConsumables.length ? selectedConsumables.map(c=><span key={c.definition.id}>{c.definition.name.replace(/([a-z\d])([A-Z])/g,'$1 $2')} <b>{Math.ceil(c.remaining)}s</b></span>) : <span className="muted">No active consumables observed</span>}</div>}</section>
        <BattleFeed scene={scene} time={time} ownerTeamId={owner.definition.teamId}/>
        <div className="fleets"><Fleet ships={ally} allShips={scene.ships} selectedId={selectedId} onSelectShip={onSelectShip}/><Fleet ships={enemy} allShips={scene.ships} selectedId={selectedId} onSelectShip={onSelectShip} enemy/></div>
      </aside>
      <BattleEnding replay={scene.replay} elapsed={endingElapsed}/>
  </div>;
}
