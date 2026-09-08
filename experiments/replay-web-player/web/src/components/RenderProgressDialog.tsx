import {useEffect,useRef,useState} from 'react';
import type {RenderProgress} from '../video/encodeMp4';
import './render-dialog.css';

export function RenderProgressDialog({progress,onCancel}:{progress:RenderProgress;onCancel:()=>void|Promise<void>}) {
  const dialog=useRef<HTMLDialogElement>(null);
  const [cancelling,setCancelling]=useState(false);
  const [error,setError]=useState<string>();
  const percent=Math.min(100,Math.round(progress.frame/Math.max(1,progress.total)*100));
  useEffect(()=>{
    const element=dialog.current!;
    element.showModal();
    return()=>element.close();
  },[]);
  return <dialog ref={dialog} className="render-dialog" aria-labelledby="render-title" aria-describedby="render-description" onCancel={e=>e.preventDefault()}>
    <div className="render-dialog-heading"><span className="render-dialog-icon" aria-hidden="true">↗</span><span>REPLAY VIDEO</span></div>
    <h2 id="render-title">{cancelling?'Stopping the render…':'Rendering your replay'}</h2>
    <p id="render-description">{cancelling?'Finishing the current frame and releasing the renderer.':'Creating the video on this computer. Keep Bridge open until it finishes.'}</p>
    <div className="render-progress-label"><span>{progress.attempt>1?`Size adjustment · pass ${progress.attempt}`:progress.frame===0?'Preparing replay…':'Rendering frames'}</span><strong>{percent}%</strong></div>
    <progress value={progress.frame} max={Math.max(1,progress.total)} aria-label="Video rendering progress"/>
    {error && <p role="alert">{error}</p>}
    <div className="render-dialog-footer"><span>{progress.frame.toLocaleString()} / {progress.total.toLocaleString()} frames</span><button autoFocus disabled={cancelling} onClick={async()=>{setCancelling(true);setError(undefined);try{await onCancel();}catch{setError('Could not cancel. Please try again.');setCancelling(false);}}}>{cancelling?'Cancelling…':'Cancel render'}</button></div>
  </dialog>;
}
