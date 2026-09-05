import { useEffect, useMemo, useRef, useState } from 'react';
import { DIVISION_COLOR, isDivisionMate, isOwnDivision } from './divisions';
import { ReplayPicker, type LocalReplaySummary } from './components/ReplayPicker';
import { TacticalMap } from './components/TacticalMap';
import { sampleScene } from './data/sampleScene';
import { battleTypeLabel, formatWowsDateTime } from './battleMeta';
import { fetchBridgeScene, listBridgeReplays } from './engine/bridgeApi';
import { loadReplayScene } from './engine/importScene';
import { evaluateScene } from './engine/timeline';
import { shipClassIconUrl, shipClassNames } from './shipClassIcons';
import { renderAndSave, webCodecsAvailable, type RenderProgress } from './video/encodeMp4';
import type { DamageEvent, EvaluatedCaptureZone, ReplayScene, ShipKnowledge, TeamDefinition } from './types';

const speeds = [1, 2, 5, 10, 20, 40];

// Picker page size: the bridge returns the newest 30 finalized replays per
// request, so a cold launch only header-reads 30 files. "Load more" pulls the
// next page.
const PAGE_SIZE = 30;

// "Render as video" is still a prototype (td-18bfca) — hidden until it ships.
// Flip to true (with WebCodecs available) to bring the button back.
const SHOW_RENDER_VIDEO = false;

// Set by vite.config.ts only for the `build:bridge` mode: talk to the
// bridge's /player/api/replays + /v1/replays routes instead of the vite-dev
// experiment's /api/* middleware (see src/engine/bridgeApi.ts). The vite-dev
// path below is otherwise unchanged.
const isBridge = import.meta.env.VITE_BRIDGE === '1';

function formatClock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  return `${Math.floor(whole / 60).toString().padStart(2, '0')}:${(whole % 60).toString().padStart(2, '0')}`;
}

function formatHealth(value: number): string {
  return new Intl.NumberFormat('en-US', { notation: 'compact', maximumFractionDigits: 1 }).format(value);
}

function knowledgeLabel(knowledge: ShipKnowledge): string {
  if (knowledge === 'spotted') return 'Spotted';
  if (knowledge === 'last-known') return 'Last known';
  return 'Unspotted';
}

function humanizeConsumable(name: string): string {
  return name.replace(/([a-z\d])([A-Z])/g, '$1 $2');
}

// One-line attribution for an HP loss: attacker + shell type/quality for gun
// hits, "Fire" for damage-over-time ticks, "Unattributed" otherwise.
function damageLabel(event: DamageEvent, nameOf: (id?: string) => string): string {
  if (event.kind === 'shell') {
    const detail = [event.ammoType, event.quality].filter(Boolean).join(' ') || 'shell hit';
    const count = event.hits && event.hits > 1 ? ` ×${event.hits}` : '';
    return `${nameOf(event.attackerId)} · ${detail}${count}`;
  }
  if (event.kind === 'fire') return 'Fire';
  return 'Unattributed';
}

// Chat is colored by audience: own team green, division yellow, all/global
// white, system muted. No channel label is shown.
function chatChannelColor(channel: string): string {
  if (channel === 'team') return '#4fe0a0';
  if (channel === 'division') return '#ffd369';
  if (channel === 'system') return '#7b9189';
  return '#eef4f1';
}

function readableName(value: string): string {
  const segments = value.split(/[\\/]/);
  return (segments[segments.length - 1] ?? value)
    .replace(/^\d+_(?:(?:NE|OC)_)?/i, '')
    .replace(/[-_]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function captureSummary(zone: EvaluatedCaptureZone, teams: TeamDefinition[]) {
  const owner = teams.find((team) => team.id === zone.owner);
  const invader = teams.find((team) => team.id === zone.invader);
  const capturing = zone.hasInvaders && Boolean(invader);
  const blocked = capturing && zone.contested;
  if (blocked) return { label: 'Blocked', ariaLabel: `${invader!.name} capture blocked at ${Math.round(zone.progress)}%`, color: '#ffbd66', phase: 'blocked' };
  if (capturing) return { label: `Capping ${Math.round(zone.progress)}%`, ariaLabel: `${invader!.name} capturing at ${Math.round(zone.progress)}%`, color: invader!.color, phase: 'capturing' };
  if (owner) return { label: '', ariaLabel: `${owner.name} held`, color: owner.color, phase: 'held' };
  return { label: '', ariaLabel: 'Neutral', color: '#7b9189', phase: 'neutral' };
}

async function fetchGeneratedScene(cacheKey = ''): Promise<ReplayScene> {
  const sceneUrl = new URL(`${import.meta.env.BASE_URL}generated/scene.json${cacheKey ? `?v=${cacheKey}` : ''}`, window.location.href).href;
  const response = await fetch(sceneUrl, { cache: 'no-store' });
  if (!response.ok) throw new Error(`Replay scene could not be loaded (HTTP ${response.status}).`);
  return loadReplayScene(await response.json(), { baseUrl: sceneUrl });
}

// The bridge/engine brand mark (teal ship-wheel), matching the app title bar.
function BrandLogo({ size = 26 }: { size?: number }) {
  return (
    <svg className="brand-logo" viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="#2fd6a6" strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 10.189V14" />
      <path d="M12 2v3" />
      <path d="M19 13V7a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v6" />
      <path d="M19.38 20A11.6 11.6 0 0 0 21 14l-8.188-3.639a2 2 0 0 0-1.624 0L3 14a11.6 11.6 0 0 0 2.81 7.76" />
      <path d="M2 21c.6.5 1.2 1 2.5 1 2.5 0 2.5-2 5-2 1.3 0 1.9.5 2.5 1s1.2 1 2.5 1c2.5 0 2.5-2 5-2 1.3 0 1.9.5 2.5 1" />
    </svg>
  );
}

export function App() {
  // The bridge opens on NO scene and shows the picker straight away (no
  // synthetic/dummy battle). The vite-dev experiment keeps its sample scene.
  const [scene, setScene] = useState<ReplayScene | null>(isBridge ? null : sampleScene);
  const [localReplays, setLocalReplays] = useState<LocalReplaySummary[]>([]);
  const [replaysTotal, setReplaysTotal] = useState(0);
  // The bridge opens straight into the picker, so it starts in a loading state.
  const [listLoading, setListLoading] = useState(isBridge);
  const [loadingMore, setLoadingMore] = useState(false);
  const [listError, setListError] = useState<string>();
  const [pickerOpen, setPickerOpen] = useState(isBridge);
  const [loadingReplayId, setLoadingReplayId] = useState<string>();
  const [replayError, setReplayError] = useState<string>();

  useEffect(() => {
    const controller = new AbortController();
    // The bridge has no generated/scene.json (that's a vite-dev-experiment
    // artifact) and starts on NO scene — a replay is chosen from the picker.
    if (!isBridge) {
      void fetchGeneratedScene().then(setScene).catch((reason) => {
        if (!controller.signal.aborted) console.info('Generated scene unavailable; using the synthetic replay.', reason);
      });
    }
    // First page. The bridge path is paginated (newest 30 + total); the vite-dev
    // path returns the whole list at once (no "load more" in the experiment).
    const firstPage: Promise<{ replays: LocalReplaySummary[]; total: number }> = isBridge
      ? listBridgeReplays({ offset: 0, limit: PAGE_SIZE, signal: controller.signal })
      : fetch('/api/replays', { cache: 'no-store', signal: controller.signal })
        .then(async (response) => {
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          const replays = (await response.json() as { replays: LocalReplaySummary[] }).replays;
          return { replays, total: replays.length };
        });
    void firstPage
      .then(({ replays, total }) => { setLocalReplays(replays); setReplaysTotal(total); })
      .catch((reason) => {
        if (controller.signal.aborted) return;
        // In the bridge the picker is the only way in, so surface the failure;
        // in the vite-dev experiment the picker is optional, so just log.
        if (isBridge) setListError(reason instanceof Error ? reason.message : String(reason));
        else console.info('Local replay picker is unavailable outside the development experiment.', reason);
      })
      .finally(() => { if (!controller.signal.aborted) setListLoading(false); });
    return () => controller.abort();
  // Initial local-scene discovery only.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // "Load more": append the next page of older replays (bridge only). Dedupes by
  // id in case the folder changed between pages.
  const loadMoreReplays = async () => {
    if (!isBridge || loadingMore || localReplays.length >= replaysTotal) return;
    setLoadingMore(true);
    setListError(undefined);
    try {
      const { replays, total } = await listBridgeReplays({ offset: localReplays.length, limit: PAGE_SIZE });
      setLocalReplays((previous) => {
        const seen = new Set(previous.map((replay) => replay.id));
        return [...previous, ...replays.filter((replay) => !seen.has(replay.id))];
      });
      setReplaysTotal(total);
    } catch (reason) {
      setListError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoadingMore(false);
    }
  };

  const chooseReplay = async (replay: LocalReplaySummary) => {
    if (loadingReplayId) return;
    setLoadingReplayId(replay.id);
    setReplayError(undefined);
    try {
      if (isBridge) {
        // The bridge decodes and returns the scene JSON directly from one GET.
        setScene(await fetchBridgeScene(replay.id));
      } else {
        const response = await fetch('/api/replays/load', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ id: replay.id }),
        });
        const payload = await response.json() as { error?: string };
        if (!response.ok) throw new Error(payload.error ?? `Replay preparation failed (HTTP ${response.status}).`);
        setScene(await fetchGeneratedScene(Date.now().toString()));
      }
      setPickerOpen(false);
    } catch (reason) {
      setReplayError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoadingReplayId(undefined);
    }
  };

  return (
    <>
      {scene ? (
        // Remount per replay so playback state resets cleanly on a new battle.
        <PlayerView
          key={scene.replay.id}
          scene={scene}
          replayCount={replaysTotal || localReplays.length}
          onOpenPicker={() => setPickerOpen(true)}
        />
      ) : (
        <main className="app-shell app-empty">
          <header className="topbar">
            <div className="brand-lockup">
              <BrandLogo />
              <div>
                <div className="eyebrow">TFD RePlayer</div>
                <h1>RePlayer</h1>
              </div>
            </div>
            <button className="choose-replay-button" onClick={() => setPickerOpen(true)}>
              <span>Choose replay</span>
              <small>{replaysTotal || 'Local'}</small>
            </button>
          </header>
          <div className="empty-hero">
            <BrandLogo size={44} />
            <p>Choose a replay from your folder to begin.</p>
            <button className="choose-replay-button" onClick={() => setPickerOpen(true)}>Choose replay</button>
          </div>
        </main>
      )}

      {pickerOpen && (
        <ReplayPicker
          replays={localReplays}
          total={replaysTotal}
          currentFilename={scene?.replay.title ?? ''}
          loadingId={loadingReplayId}
          loading={listLoading}
          loadingMore={loadingMore}
          canLoadMore={isBridge && localReplays.length < replaysTotal}
          pageSize={PAGE_SIZE}
          onLoadMore={loadMoreReplays}
          error={replayError ?? listError}
          onChoose={chooseReplay}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </>
  );
}

function PlayerView({ scene, replayCount, onOpenPicker }: { scene: ReplayScene; replayCount: number; onOpenPicker: () => void }) {
  const [time, setTime] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(10);
  const [selectedShipId, setSelectedShipId] = useState(
    scene.replay.perspectiveEntityId
      ?? scene.ships.find((ship) => ship.relation === 'self')?.id
      ?? scene.ships[0].id,
  );
  const previousFrame = useRef<number | undefined>(undefined);
  const state = useMemo(() => evaluateScene(scene, time), [scene, time]);

  // Experimental: render this replay to a 16:9 broadcast-layout mp4 (offline,
  // bridge-local). Prototype — see td-18bfca.
  const [renderProgress, setRenderProgress] = useState<RenderProgress | null>(null);
  const [renderNote, setRenderNote] = useState<string>();
  const onRenderVideo = async () => {
    if (renderProgress) return;
    setRenderNote(undefined);
    setRenderProgress({ frame: 0, total: 1 });
    const base = (scene.replay.title || perspectiveShip?.shipName || 'replay')
      .replace(/[^\w.-]+/g, '_').slice(0, 60) + '_' + readableName(scene.map.name).replace(/[^\w.-]+/g, '_');
    try {
      const saved = await renderAndSave(scene, base, (p) => setRenderProgress(p));
      const secs = (saved.elapsedMs / 1000).toFixed(1);
      const mb = (saved.bytes / 1_048_576).toFixed(1);
      setRenderNote(`Rendered ${saved.frames} frames in ${secs}s → ${mb} MB${saved.path ? ` · ${saved.path}` : ' · downloaded'}`);
    } catch (reason) {
      setRenderNote(`Render failed: ${reason instanceof Error ? reason.message : String(reason)}`);
    } finally {
      setRenderProgress(null);
    }
  };

  useEffect(() => {
    if (!playing) {
      previousFrame.current = undefined;
      return;
    }
    let frame = 0;
    const tick = (now: number) => {
      const previous = previousFrame.current ?? now;
      previousFrame.current = now;
      const delta = Math.min(0.1, (now - previous) / 1000);
      setTime((current) => Math.min(scene.replay.duration, current + delta * speed));
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing, scene.replay.duration, speed]);

  useEffect(() => {
    if (time >= scene.replay.duration) setPlaying(false);
  }, [scene.replay.duration, time]);

  const selected = state.ships.find((ship) => ship.definition.id === selectedShipId) ?? state.ships[0];
  const perspectiveShip = scene.ships.find((ship) => ship.id === scene.replay.perspectiveEntityId)
    ?? scene.ships.find((ship) => ship.relation === 'self');
  const selectedDamage = scene.damage.filter((event) => event.targetId === selected.definition.id && event.t <= time).slice(-6).reverse();
  const shipNameById = useMemo(() => new Map(scene.ships.map((ship) => [ship.id, ship.clan ? `[${ship.clan}] ${ship.shipName}` : ship.shipName])), [scene]);
  const nameOf = (id?: string) => (id && shipNameById.get(id)) || 'Unknown';
  const selectedConsumables = state.consumables.filter((activation) => activation.definition.shipId === selected.definition.id);
  const visibleChat = (scene.chat ?? []).filter((message) => message.t <= time).slice(-40);
  const sortedCaps = state.captureZones
    .filter((zone) => zone.enabled)
    .sort((left, right) => left.definition.label.localeCompare(right.definition.label));

  const seek = (next: number) => setTime(Math.max(0, Math.min(scene.replay.duration, next)));
  const togglePlayback = () => {
    if (time >= scene.replay.duration) setTime(0);
    setPlaying((current) => !current);
  };

  // #5: title eyebrow shows the battle type + date/time, and flags an
  // incomplete recording (player left before the battle ended).
  const typeLabel = battleTypeLabel(scene.replay.battleType);
  const playedAt = formatWowsDateTime(scene.replay.dateTime);
  const eyebrow = [typeLabel, playedAt].filter(Boolean).join(' · ') || 'Battle replay';

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-lockup">
          <BrandLogo />
          <div>
            <div className="eyebrow">
              <span>{eyebrow}</span>
              {scene.replay.complete === false && <span className="incomplete-badge" title="The player left before the battle ended — this recording is missing the end of the battle.">Incomplete</span>}
            </div>
            <h1>{perspectiveShip?.shipName ?? 'Replay'} <span>· {readableName(scene.map.name)}</span></h1>
          </div>
        </div>
        <div className="topbar-actions">
          {SHOW_RENDER_VIDEO && webCodecsAvailable() && (
            <button
              className="render-video-button"
              onClick={onRenderVideo}
              disabled={Boolean(renderProgress)}
              title="Render this replay to a 16:9 broadcast-layout mp4 (experimental)"
            >
              {renderProgress
                ? `Rendering… ${Math.round((renderProgress.frame / renderProgress.total) * 100)}%`
                : 'Render as video'}
            </button>
          )}
          <button className="choose-replay-button" onClick={onOpenPicker}>
            <span>Choose replay</span>
            <small>{replayCount || 'Local'}</small>
          </button>
        </div>
      </header>
      {renderNote && <div className="render-note" role="status">{renderNote}</div>}

      <section className="scoreboard" aria-label="Team score and capture points">
        {scene.teams.map((team, index) => (
          <div className={`score-team ${index === 1 ? 'enemy' : ''}`} key={team.id}>
            <div className="score-name"><span className="team-dot" style={{ background: team.color }} />{team.name}</div>
            <strong style={{ color: team.color }}>{state.scores[team.id]}</strong>
          </div>
        ))}
        <div className="cap-strip">
          {sortedCaps.map((zone) => {
            const summary = captureSummary(zone, scene.teams);
            return (
              <div className={`cap-summary ${summary.phase}`} key={zone.definition.id} aria-label={`${zone.definition.label}: ${summary.ariaLabel}`}>
                <span style={{ '--cap-color': summary.color } as React.CSSProperties}>{zone.definition.label}</span>
                {summary.label && <small>{summary.label}</small>}
              </div>
            );
          })}
        </div>
      </section>

      <section className="workspace">
        <aside className="panel roster-panel">
          <div className="panel-heading">
            <span>Battle roster</span>
            <small>{scene.ships.length} ships</small>
          </div>
          <div className="rosters">
            {scene.teams.map((team) => (
              <div className="roster-team" key={team.id}>
                <div className="roster-team-heading" style={{ color: team.color }}>{team.name}</div>
                {state.ships.filter((ship) => ship.definition.teamId === team.id).map((ship) => {
                  const healthRatio = ship.health / ship.definition.maxHealth;
                  const divisionMate = isDivisionMate(ship.definition, scene.ships);
                  return (
                    <button
                      className={`roster-row ${ship.definition.id === selectedShipId ? 'selected' : ''} ${ship.knowledge}`}
                      key={ship.definition.id}
                      onClick={() => setSelectedShipId(ship.definition.id)}
                    >
                      <span className={`class-badge ${isOwnDivision(ship.definition, scene.ships) ? 'division-mate' : ''} ${ship.definition.id === selectedShipId ? 'selected-ship' : ''}`} title={shipClassNames[ship.definition.shipClass]}>
                        <img src={shipClassIconUrl(ship.definition.shipClass)} alt={shipClassNames[ship.definition.shipClass]} />
                      </span>
                      <span className="roster-copy">
                        <strong>
                          {ship.definition.divisionLabel && (
                            <span
                              className="division-badge"
                              style={{ color: divisionMate || ship.definition.relation === 'self' ? DIVISION_COLOR : team.color }}
                              title={`Division ${ship.definition.divisionLabel}${divisionMate ? ' · Your division' : ''}`}
                            >{ship.definition.divisionLabel}</span>
                          )}
                          {ship.definition.shipName}
                        </strong>
                        <small>{ship.definition.clan ? `[${ship.definition.clan}] ` : ''}{ship.definition.playerName}</small>
                        <i><b style={{ width: `${Math.max(0, healthRatio * 100)}%`, background: team.color }} /></i>
                      </span>
                      <span className={`visibility-dot ${ship.knowledge}`} title={knowledgeLabel(ship.knowledge)} />
                    </button>
                  );
                })}
              </div>
            ))}
          </div>
        </aside>

        <section className="viewer-column">
          <div className="map-frame">
            <TacticalMap
              scene={scene}
              time={time}
              selectedShipId={selectedShipId}
              onSelectShip={setSelectedShipId}
            />
            <div className="legend">
              <span><i className="legend-spotted" /> Spotted</span>
              <span><i className="legend-last" /> Last known</span>
            </div>
          </div>

          <div className="transport">
            <div className="transport-buttons">
              <button onClick={() => seek(time - 10)} aria-label="Back 10 seconds">−10</button>
              <button className="play-button" onClick={togglePlayback} aria-label={playing ? 'Pause' : 'Play'}>{playing ? 'Ⅱ' : '▶'}</button>
              <button onClick={() => seek(time + 10)} aria-label="Forward 10 seconds">+10</button>
            </div>
            <span className="timecode current">{formatClock(time)}</span>
            <input
              aria-label="Replay position"
              type="range"
              min="0"
              max={scene.replay.duration}
              step="0.05"
              value={time}
              onChange={(event) => seek(Number(event.target.value))}
              style={{ '--progress': `${(time / scene.replay.duration) * 100}%` } as React.CSSProperties}
            />
            <span className="timecode">{formatClock(scene.replay.duration)}</span>
            <div className="speed-buttons" aria-label="Playback speed">
              {speeds.map((option) => <button key={option} className={speed === option ? 'active' : ''} onClick={() => setSpeed(option)}>{option}×</button>)}
            </div>
          </div>
        </section>

        <aside className="panel detail-panel">
          <div className="panel-heading">
            <span>Selected ship</span>
            <span className={`status-tag ${selected.knowledge}`}>{knowledgeLabel(selected.knowledge)}</span>
          </div>
          <div className="ship-identity">
            <span className="large-class"><img src={shipClassIconUrl(selected.definition.shipClass)} alt={shipClassNames[selected.definition.shipClass]} /></span>
            <div>
              <h2>{selected.definition.shipName}</h2>
              <p>{selected.definition.clan ? `[${selected.definition.clan}] ` : ''}{selected.definition.playerName}</p>
            </div>
          </div>
          <div className="health-card">
            <div><span>Ship health</span><strong>{Math.round((selected.health / selected.definition.maxHealth) * 100)}%</strong></div>
            <div className="large-health"><i style={{ width: `${Math.max(0, (selected.health / selected.definition.maxHealth) * 100)}%` }} /></div>
            <small>{formatHealth(selected.health)} / {formatHealth(selected.definition.maxHealth)} HP</small>
          </div>
          <div className="metric-grid">
            <div><span>Heading</span><strong>{Math.round(selected.pose.yaw).toString().padStart(3, '0')}°</strong></div>
            <div><span>Visibility</span><strong>{knowledgeLabel(selected.knowledge)}</strong></div>
            {selected.definition.relation === 'self' && (
              <div className="wide-metric"><span>Spotted by enemy</span><strong className={selected.detectedByEnemy ? 'detected-copy' : ''}>{selected.detectedByEnemy ? 'Spotted' : 'Not spotted'}</strong></div>
            )}
          </div>
          {selectedConsumables.length > 0 && (
            <>
              <div className="event-heading"><span>Active consumables</span><small>right now</small></div>
              <div className="consumable-chips">
                {selectedConsumables.map((activation) => (
                  <span className="consumable-chip" key={activation.definition.id}>
                    {humanizeConsumable(activation.definition.name)}
                    <small>{Math.ceil(activation.remaining)}s</small>
                  </span>
                ))}
              </div>
            </>
          )}
          <div className="event-heading"><span>Recent damage taken</span><small>up to cursor</small></div>
          <div className="event-list">
            {selectedDamage.length ? selectedDamage.map((event) => (
              <div className={`event-row damage-${event.kind}`} key={event.id}>
                <span>{formatClock(event.t)}</span>
                <div><strong>−{formatHealth(event.amount)} HP</strong><small>{damageLabel(event, nameOf)}</small></div>
              </div>
            )) : <div className="empty-event">No damage recorded yet.</div>}
          </div>
          <div className="event-heading"><span>Battle chat</span><small>up to cursor</small></div>
          <div className="chat-log">
            {visibleChat.length ? visibleChat.map((message) => (
              <div className="chat-line" key={message.id} style={{ color: chatChannelColor(message.channel) }}>
                <span className="chat-time">{formatClock(message.t)}</span>
                <span className="chat-body"><b>{message.senderName}</b> {message.message}</span>
              </div>
            )) : <div className="empty-event">No chat yet.</div>}
          </div>
        </aside>
      </section>
    </main>
  );
}
