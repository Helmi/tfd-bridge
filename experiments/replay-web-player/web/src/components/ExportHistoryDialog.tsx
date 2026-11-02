import {useEffect, useRef} from 'react';
import {formatExportLine, formatExportTime, type ExportHistoryEntry} from '../video/exportHistory';
import './render-dialog.css';

/** All stored exports, newest first, in a scrollable list. Opens on top of the export dialog. */
export function ExportHistoryDialog({entries, onClose}: {entries: ExportHistoryEntry[]; onClose: () => void}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {const element = dialog.current!; element.showModal(); return () => element.close();}, []);
  // Esc closes only this view: the cancel event must not reach the export dialog underneath.
  return <dialog ref={dialog} className="render-dialog export-history-dialog" aria-labelledby="export-history-title"
    onCancel={event => {event.preventDefault(); onClose();}}>
    <div className="render-dialog-heading">LOCAL VIDEO · MP4</div>
    <h2 id="export-history-title">Recent exports ({entries.length})</h2>
    <div className="video-history export-history-list">
      <ul>{entries.map((entry, i) => {
        const {main, rest} = formatExportLine(entry);
        return <li key={`${entry.at}-${i}`}>
          <span className="export-history-head"><strong>{main}</strong><time dateTime={entry.at}>{formatExportTime(entry.at)}</time></span>
          <small>{rest}</small>
        </li>;
      })}</ul>
    </div>
    <div className="render-dialog-footer"><button type="button" onClick={onClose}>Close</button></div>
  </dialog>;
}
