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
  /** Total finalized replays on disk (loaded ones may be fewer — see canLoadMore). */
  total: number;
  currentFilename: string;
  loadingId?: string;
  /** Initial page is still loading (nothing to show yet). */
  loading?: boolean;
  /** A "Load more" page is in flight. */
  loadingMore?: boolean;
  /** More pages exist on disk beyond what's loaded. */
  canLoadMore?: boolean;
  /** How many a "Load more" click pulls (for the button label). */
  pageSize?: number;
  error?: string;
  onChoose: (replay: LocalReplaySummary) => void;
  onLoadMore?: () => void;
  onClose: () => void;
}

const dateFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
  timeStyle: 'short',
});

export function ReplayPicker({
  replays,
  total,
  currentFilename,
  loadingId,
  loading,
  loadingMore,
  canLoadMore,
  pageSize = 30,
  error,
  onChoose,
  onLoadMore,
  onClose,
}: Props) {
  const [query, setQuery] = useState('');
  const searching = query.trim().length > 0;
  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) return replays;
    return replays.filter((replay) => `${replay.shipName} ${replay.mapName} ${replay.filename}`.toLocaleLowerCase().includes(needle));
  }, [query, replays]);
  const busy = Boolean(loadingId);
  const decoding = loadingId ? replays.find((replay) => replay.id === loadingId) : undefined;

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
          <span>{loading ? 'Loading…' : searching ? `${filtered.length} match` : `${total} replays`}</span>
        </div>
        {error && <div className="picker-error">{error}</div>}
        <div className="replay-list">
          {loading && !replays.length ? (
            <div className="picker-loading" role="status">
              <span className="picker-spinner" aria-hidden="true" />
              Loading replays…
            </div>
          ) : (
          <>
          {filtered.map((replay) => {
            const active = replay.filename === currentFilename;
            const preparing = replay.id === loadingId;
            const typeLabel = battleTypeLabel(replay.battleType);
            const incomplete = replay.complete === false;
            return (
              <button
                key={replay.id}
                className={`replay-option ${active ? 'active' : ''} ${incomplete ? 'incomplete' : ''}`}
                onClick={() => onChoose(replay)}
                disabled={Boolean(loadingId)}
              >
                <span className="replay-class-icon" title={shipClassNames[replay.shipClass]}>
                  <img src={shipClassIconUrl(replay.shipClass)} alt={shipClassNames[replay.shipClass]} />
                </span>
                <span className="replay-option-copy">
                  <strong>{replay.shipName}</strong>
                  <small>Map: {replay.mapName}</small>
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
                  <small>{preparing ? 'Preparing replay…' : active ? 'Currently loaded' : `${(replay.size / 1_048_576).toFixed(1)} MB`}</small>
                </span>
              </button>
            );
          })}
          {!filtered.length && (
            <div className="empty-replays">
              {searching ? 'No matches in the loaded replays.' : 'No replays found.'}
            </div>
          )}
          {canLoadMore && (
            <button
              className="load-more-button"
              onClick={onLoadMore}
              disabled={loadingMore || busy}
            >
              {loadingMore ? 'Loading…' : `Load ${Math.min(pageSize, total - replays.length)} more`}
            </button>
          )}
          </>
          )}
        </div>
        <footer>Preparing a replay decodes it locally; nothing is uploaded.</footer>

        {loadingId && (
          <div className="picker-decoding" role="status" aria-live="polite">
            <span className="picker-spinner picker-spinner-lg" aria-hidden="true" />
            <strong>Decoding replay…</strong>
            {decoding && <span className="picker-decoding-name">{decoding.shipName} · {decoding.mapName}</span>}
            <small>Reading the full battle from the replay file — this takes a few seconds.</small>
          </div>
        )}
      </section>
    </div>
  );
}
