import { useMemo, useState } from 'react';
import { battleTypeLabel } from '../battleMeta';
import { shipClassIconUrl, shipClassNames, type ShipClass } from '../shipClassIcons';

export interface LocalReplaySummary {
  id: string;
  filename: string;
  shipName: string;
  shipClass: ShipClass;
  mapName: string;
  playedAt: string;
  modifiedAt: string;
  size: number;
  /** Raw WoWS `matchGroup` (mapped to a label for display), when the bridge provides it. */
  battleType?: string;
  /** Short "major.minor" client version, e.g. "15.5". */
  gameVersion?: string;
  /** False when the recording ended before the battle did (early exit). */
  complete?: boolean;
}

interface Props {
  replays: LocalReplaySummary[];
  currentFilename: string;
  loadingId?: string;
  error?: string;
  onChoose: (replay: LocalReplaySummary) => void;
  onClose: () => void;
}

const dateFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
  timeStyle: 'short',
});

export function ReplayPicker({ replays, currentFilename, loadingId, error, onChoose, onClose }: Props) {
  const [query, setQuery] = useState('');
  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) return replays;
    return replays.filter((replay) => `${replay.shipName} ${replay.mapName} ${replay.filename}`.toLocaleLowerCase().includes(needle));
  }, [query, replays]);

  return (
    <div className="replay-picker-backdrop" onMouseDown={(event) => event.target === event.currentTarget && !loadingId && onClose()}>
      <section className="replay-picker" role="dialog" aria-modal="true" aria-label="Choose a local replay">
        <header>
          <div>
            <span className="eyebrow">Local replay folder</span>
            <h2>Choose a battle</h2>
          </div>
          <button className="picker-close" onClick={onClose} disabled={Boolean(loadingId)} aria-label="Close replay picker">×</button>
        </header>
        <div className="picker-search">
          <input
            autoFocus
            type="search"
            placeholder="Search ship, map, or filename"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          <span>{filtered.length} replays</span>
        </div>
        {error && <div className="picker-error">{error}</div>}
        <div className="replay-list">
          {filtered.map((replay) => {
            const active = replay.filename === currentFilename;
            const loading = replay.id === loadingId;
            const typeLabel = battleTypeLabel(replay.battleType);
            const incomplete = replay.complete === false;
            return (
              <button
                key={replay.id}
                className={`replay-option ${active ? 'active' : ''} ${incomplete ? 'incomplete' : ''}`}
                onClick={() => onChoose(replay)}
                disabled={Boolean(loadingId)}
              >
                <span className="replay-class-icon"><img src={shipClassIconUrl(replay.shipClass)} alt="" /></span>
                <span className="replay-option-copy">
                  <strong>{replay.shipName}</strong>
                  <small>{shipClassNames[replay.shipClass]} · {replay.mapName}</small>
                  {(typeLabel || replay.gameVersion || incomplete) && (
                    <small className="replay-tags">
                      {typeLabel && <span className="replay-tag">{typeLabel}</span>}
                      {replay.gameVersion && <span className="replay-tag">v{replay.gameVersion}</span>}
                      {incomplete && <span className="replay-tag incomplete-tag" title="The player left before the battle ended — this recording is missing the end of the battle.">Incomplete</span>}
                    </small>
                  )}
                </span>
                <span className="replay-option-meta">
                  <time>{dateFormatter.format(new Date(replay.playedAt))}</time>
                  <small>{loading ? 'Preparing replay…' : active ? 'Currently loaded' : `${(replay.size / 1_048_576).toFixed(1)} MB`}</small>
                </span>
              </button>
            );
          })}
          {!filtered.length && <div className="empty-replays">No matching replays found.</div>}
        </div>
        <footer>Preparing a replay decodes it locally; nothing is uploaded.</footer>
      </section>
    </div>
  );
}
