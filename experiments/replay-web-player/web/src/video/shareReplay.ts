import { useEffect, useState } from 'react';

type ShareStatus = {state:string;frame?:number;total?:number;attempt?:number;job_id?:string;error?:string;message_url?:string};
const storageKey='tfd-active-video-share';
const safeStartError=(status:number)=>({
  400:'This replay cannot be shared.',
  409:'This replay cannot be shared. Check that it is complete and belongs to the connected player.',
  403:'Sharing is no longer authorized. Start again.',
}[status] ?? 'Could not start sharing.');

export const shareFailureCopy=(error?:string)=>error==='recorder_account_mismatch' ? 'This replay was recorded by another player.'
  :error==='recorder_realm_mismatch' ? 'Engine could not confirm this replay for the connected account.'
  :error==='not_battle_owner' ? 'Engine could not confirm ownership for this battle.'
  :error==='capability_revoked' ? 'Sharing is no longer authorized. Start again.'
  :error==='account_lifecycle_changed' ? 'Your connected account changed. Start sharing again.'
  :error==='preflight_expired' || error==='preflight_invalid' ? 'Sharing authorization expired. Start again.'
  :error==='replay_replaced' ? 'The replay changed before rendering. Start again.'
  :'Please try again.';

export function useReplayShare() {
  const [id,setId]=useState<string|null>(()=>sessionStorage.getItem(storageKey));
  const [status,setStatus]=useState<ShareStatus|null>(null);
  const [authorizationUrl,setAuthorizationUrl]=useState<string>();
  const [starting,setStarting]=useState(false);
  const [browserError,setBrowserError]=useState<string>();
  const [opening,setOpening]=useState(false);
  useEffect(()=>{
    if(!id)return;
    let stopped=false;const abort=new AbortController();
    void (async()=>{
      const deadline=Date.now()+10*60*1000;
      while(!stopped){
        try{
          const response=await fetch(`/v1/share/status/${encodeURIComponent(id)}`,{signal:abort.signal});
          if(response.status===404){
            if(Date.now()>deadline)throw new Error('Authorization expired. Try sharing again.');
            setStatus({state:'awaiting_authorization'});
          }else{
            if(!response.ok)throw new Error('Sharing status is unavailable.');
            const next=await response.json() as ShareStatus;
            if(stopped)return;
            setStatus(next);setAuthorizationUrl(undefined);setBrowserError(undefined);
            if(['posted','failed','expired'].includes(next.state)){
              sessionStorage.removeItem(storageKey);setId(null);return;
            }
          }
        }catch(error){
          if(stopped)return;
          setStatus({state:'failed',error:error instanceof Error?error.message:'Sharing failed.'});
          sessionStorage.removeItem(storageKey);setId(null);return;
        }
        await new Promise(resolve=>setTimeout(resolve,1500));
      }
    })();
    return()=>{stopped=true;abort.abort();};
  },[id]);
  const start=async(replayName:string,arena:string)=>{
    setStarting(true);setStatus(null);setAuthorizationUrl(undefined);setBrowserError(undefined);
    try{
      const response=await fetch('/v1/share/authorize',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({replay_name:replayName,arena_unique_id:arena})});
      if(!response.ok)throw new Error(safeStartError(response.status));
      const result=await response.json() as {state:string;authorization_url:string;opened:boolean};
      const url=new URL(result.authorization_url);
      if(url.origin!=='https://engine.tfd.rocks'||url.pathname!=='/share/video/authorize')throw new Error('Invalid authorization address.');
      setAuthorizationUrl(url.href);sessionStorage.setItem(storageKey,result.state);setId(result.state);
      if(!result.opened)setBrowserError('Could not open your default browser. Retry opening it below.');
      setStatus({state:'awaiting_authorization'});
    }catch(error){setStatus({state:'failed',error:error instanceof Error?error.message:'Sharing failed.'});}
    finally{setStarting(false);}
  };
  const openAuthorization=async()=>{
    if(!id || opening)return;
    setOpening(true);setBrowserError(undefined);
    try{
      const response=await fetch('/v1/share/authorize/open',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({state:id})});
      if(response.status===403){
        sessionStorage.removeItem(storageKey);setId(null);setAuthorizationUrl(undefined);
        setStatus({state:'failed',error:'Authorization expired. Start sharing again.'});
        return;
      }
      if(!response.ok)throw new Error('Could not open your default browser. Please retry.');
      const result=await response.json() as {opened:boolean};
      if(!result.opened)throw new Error('Could not open your default browser. Please retry.');
    }catch(error){setBrowserError(error instanceof Error?error.message:'Could not open your default browser.');}
    finally{setOpening(false);}
  };
  const safeFailure=shareFailureCopy(status?.error);
  const label=status?.state==='rendering'?`Rendering video… ${Math.round((status.frame??0)/Math.max(1,status.total??1)*100)}%`
    :status?.state==='awaiting_authorization'?'Confirm sharing in Engine.'
    :status?.state==='failed'?`Sharing failed: ${safeFailure}`
    :status?.state==='posted'?'Video posted to Discord.'
    :status?.state==='queued'?'Video uploaded; waiting for Discord delivery.'
    :status?({authorizing:'Checking authorization…',resolving:'Finding the local replay…',uploading:'Uploading video…',posting:'Posting to Discord…',expired:'Sharing authorization expired.'}[status.state]??status.state):undefined;
  const renderProgress=status?.state==='rendering'?{frame:status.frame??0,total:Math.max(1,status.total??1),attempt:status.attempt??1}:null;
  const cancelRender=async()=>{
    if(!status?.job_id)return;
    const response=await fetch(`/v1/render/jobs/${encodeURIComponent(status.job_id)}/cancel`,{method:'POST'});
    if(!response.ok)throw new Error('Could not cancel the render.');
  };
  return{start,busy:starting||id!==null,label,authorizationUrl,openAuthorization,opening,browserError,messageUrl:status?.message_url,renderProgress,cancelRender};
}
