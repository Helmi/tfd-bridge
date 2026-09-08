import { useEffect, useState } from 'react';

type ShareStatus = {state:string;frame?:number;total?:number;attempt?:number;job_id?:string;error?:string;message_url?:string};
const storageKey='tfd-active-video-share';
export function useReplayShare() {
  const [id,setId]=useState<string|null>(()=>sessionStorage.getItem(storageKey));
  const [status,setStatus]=useState<ShareStatus|null>(null);
  const [authorizationUrl,setAuthorizationUrl]=useState<string>();
  const [starting,setStarting]=useState(false);
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
            setStatus(next);setAuthorizationUrl(undefined);
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
    setStarting(true);setStatus(null);setAuthorizationUrl(undefined);
    try{
      const response=await fetch('/v1/share/authorize',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({replay_name:replayName,arena_unique_id:arena})});
      if(!response.ok)throw new Error(`Could not start sharing (${response.status}).`);
      const result=await response.json() as {state:string;authorization_url:string;opened:boolean};
      const url=new URL(result.authorization_url);
      if(url.origin!=='https://engine.tfd.rocks'||url.pathname!=='/share/video/authorize')throw new Error('Invalid authorization address.');
      setAuthorizationUrl(url.href);sessionStorage.setItem(storageKey,result.state);setId(result.state);
      setStatus({state:'awaiting_authorization'});
    }catch(error){setStatus({state:'failed',error:error instanceof Error?error.message:'Sharing failed.'});}
    finally{setStarting(false);}
  };
  const label=status?.state==='rendering'?`Rendering video… ${Math.round((status.frame??0)/Math.max(1,status.total??1)*100)}%`
    :status?.state==='awaiting_authorization'?'Confirm sharing in Engine.'
    :status?.state==='failed'?`Sharing failed: ${status.error??'Please try again.'}`
    :status?.state==='posted'?'Video posted to Discord.'
    :status?.state==='queued'?'Video uploaded; waiting for Discord delivery.'
    :status?({authorizing:'Checking authorization…',resolving:'Finding the local replay…',uploading:'Uploading video…',posting:'Posting to Discord…',expired:'Sharing authorization expired.'}[status.state]??status.state):undefined;
  const renderProgress=status?.state==='rendering'?{frame:status.frame??0,total:Math.max(1,status.total??1),attempt:status.attempt??1}:null;
  const cancelRender=async()=>{
    if(!status?.job_id)return;
    const response=await fetch(`/v1/render/jobs/${encodeURIComponent(status.job_id)}/cancel`,{method:'POST'});
    if(!response.ok)throw new Error('Could not cancel the render.');
  };
  return{start,busy:starting||id!==null,label,authorizationUrl,messageUrl:status?.message_url,renderProgress,cancelRender};
}
