import { useMemo, useState } from 'react';
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
            return (
              <button
                key={replay.id}
                className={`replay-option ${active ? 'active' : ''}`}
                onClick={() => onChoose(replay)}
                disabled={Boolean(loadingId)}
              >
                <span className="replay-class-icon"><img src={shipClassIconUrl(replay.shipClass)} alt="" /></span>
                <span className="replay-option-copy">
                  <strong>{replay.shipName}</strong>
                  <small>{shipClassNames[replay.shipClass]} · {replay.mapName}</small>
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
