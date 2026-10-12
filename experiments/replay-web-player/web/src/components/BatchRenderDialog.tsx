import {useEffect,useRef,useState} from 'react';
import type {RenderProgress} from '../video/encodeMp4';
import type {BatchReplay,BatchResult} from '../video/batchRender';
import './render-dialog.css';

const label=(replay:BatchReplay)=>`${replay.shipName} · ${replay.mapName}`;

export function BatchRenderDialog({total,index,current,progress,results,onCancel,onClose}:{
  total:number;index:number;current?:BatchReplay;progress:RenderProgress;
  /** Set once the batch has finished or was cancelled. */
  results?:BatchResult[];onCancel:()=>void;onClose:()=>void;
}) {
  const dialog=useRef<HTMLDialogElement>(null);
  const [cancelling,setCancelling]=useState(false);
  const percent=Math.min(100,Math.round(progress.frame/Math.max(1,progress.total)*100));
  useEffect(()=>{
    const element=dialog.current!;
    element.showModal();
    return()=>element.close();
  },[]);
  return <dialog ref={dialog} className="render-dialog" aria-labelledby="batch-title" onCancel={e=>e.preventDefault()}>
    <div className="render-dialog-heading"><span className="render-dialog-icon" aria-hidden="true">↗</span><span>REPLAY VIDEOS</span></div>
    {results ? <>
      <h2 id="batch-title">{results.filter(r=>r.status==='saved').length} of {results.length} saved</h2>
      <ul className="batch-render-list">{results.map(r=><li key={r.replay.id}>
        <span>{label(r.replay)}</span>
        <strong className={`batch-${r.status}`}>{r.status==='saved'?'Saved':r.status==='skipped'?'Skipped':`Failed · ${r.error}`}</strong>
      </li>)}</ul>
      <p>Videos are in your <strong>Videos → TFD Bridge Renders</strong> folder.</p>
      <div className="render-dialog-footer"><span/><button autoFocus onClick={onClose}>Close</button></div>
    </> : <>
      <h2 id="batch-title">{cancelling?'Stopping the batch…':`Battle ${index+1} of ${total}`}</h2>
      <p>{current?label(current):'Preparing…'}<br/>Creating the videos on this computer. Keep Bridge open until it finishes.</p>
      <div className="render-progress-label"><span>{progress.attempt>1?`Size adjustment · pass ${progress.attempt}`:progress.frame===0?'Preparing replay…':'Rendering frames'}</span><strong>{percent}%</strong></div>
      <progress value={progress.frame} max={Math.max(1,progress.total)} aria-label="Video rendering progress"/>
      <div className="render-dialog-footer"><span>{progress.frame.toLocaleString()} / {progress.total.toLocaleString()} frames</span><button autoFocus disabled={cancelling} onClick={()=>{setCancelling(true);onCancel();}}>{cancelling?'Cancelling…':'Cancel all'}</button></div>
    </>}
  </dialog>;
}
