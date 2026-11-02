import {useEffect, useRef} from 'react';
import {formatExportRow, type ExportHistoryEntry} from '../video/exportHistory';
import './render-dialog.css';

const COLUMNS: [string, boolean][] = [['When', false], ['Replay', false], ['Format', false], ['Encoder', false], ['Bitrate', false], ['Speed', false], ['Video', true], ['Render', true], ['FPS', true], ['Size', true]];

/** All stored exports, newest first, in a scrollable list. Opens on top of the export dialog. */
export function ExportHistoryDialog({entries, onClose}: {entries: ExportHistoryEntry[]; onClose: () => void}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {const element = dialog.current!; element.showModal(); return () => element.close();}, []);
  // Esc closes only this view. React bubbles synthetic events through the component tree,
  // so without stopPropagation the export dialog's onCancel (its React parent) also fires.
  return <dialog ref={dialog} className="render-dialog export-history-dialog" aria-labelledby="export-history-title"
    onCancel={event => {event.preventDefault(); event.stopPropagation(); onClose();}}>
    <div className="render-dialog-heading">LOCAL VIDEO · MP4</div>
    <h2 id="export-history-title">Recent exports ({entries.length})</h2>
    <div className="video-history export-history-list">
      <table className="export-history-table">
        <thead><tr>{COLUMNS.map(([label, numeric]) => <th key={label} scope="col" className={numeric ? 'num' : undefined}>{label}</th>)}</tr></thead>
        <tbody>{entries.map((entry, i) => {
          const row = formatExportRow(entry);
          return <tr key={`${entry.at}-${i}`}>
            <td className="when"><time dateTime={entry.at}>{row.when}</time></td>
            <td className="replay" title={`${row.ship} ${row.map}`}><strong>{row.ship}</strong> <span>{row.map}</span></td>
            <td>{row.format}</td>
            <td>{row.encoder}</td>
            <td>{row.bitrate}</td>
            <td>{row.speed}</td>
            <td className="num">{row.video}</td>
            <td className="num">{row.render}</td>
            <td className="num">{row.fps}</td>
            <td className="num">{row.size}</td>
          </tr>;
        })}</tbody>
      </table>
    </div>
    <div className="render-dialog-footer"><button type="button" onClick={onClose}>Close</button></div>
  </dialog>;
}
