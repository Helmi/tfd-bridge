// Fit generated RGB texture to original island components. This function never
// supplies alpha: every final coverage sample is copied from the game asset.
module.exports = function registerLand(source, paint, size) {
  const count=size*size, originalSize=size/2;
  const output=new Uint8ClampedArray(count*4);
  const covered=new Uint8Array(count), clean=new Uint8Array(count);
  const neighbours=(p,visit)=>{
    const x=p%size;
    if(x)visit(p-1);if(x+1<size)visit(p+1);
    if(p>=size)visit(p-size);if(p+size<count)visit(p+size);
  };
  const queue=new Int32Array(count);
  let sourceCoveredPixels=0;
  for(let p=0;p<count;p++) {
    const src=(Math.floor(Math.floor(p/size)/2)*originalSize+Math.floor(p%size/2))*4;
    output[p*4+3]=source[src+3];
    if(source[src+3]){covered[p]=1;sourceCoveredPixels++;}
    const r=paint[p*4],g=paint[p*4+1],b=paint[p*4+2];
    clean[p]=paint[p*4+3]>200 && !(r>g+5 && b>g+5) ? 1 : 0;
  }
  // Remove the key's antialiased fringe before any colour is extended. Merely
  // rejecting saturated magenta allows muted pink pixels to contaminate coasts.
  for(let iteration=0;iteration<5;iteration++) {
    let tail=0;
    for(let p=0;p<count;p++) if(clean[p]) {
      let edge=false;neighbours(p,n=>{if(!clean[n])edge=true;});
      if(edge)queue[tail++]=p;
    }
    for(let i=0;i<tail;i++)clean[queue[i]]=0;
  }
  const components=mask=>{
    const labels=new Int32Array(count),parts=[];
    for(let start=0;start<count;start++) if(mask[start]&&!labels[start]) {
      const label=parts.length+1,part={label,pixels:[],minX:size,minY:size,maxX:0,maxY:0,cx:0,cy:0};
      let head=0,tail=0;queue[tail++]=start;labels[start]=label;
      while(head<tail) {
        const p=queue[head++],x=p%size,y=Math.floor(p/size);
        part.pixels.push(p);part.minX=Math.min(part.minX,x);part.maxX=Math.max(part.maxX,x);
        part.minY=Math.min(part.minY,y);part.maxY=Math.max(part.maxY,y);part.cx+=x;part.cy+=y;
        neighbours(p,n=>{if(mask[n]&&!labels[n]){labels[n]=label;queue[tail++]=n;}});
      }
      part.area=part.pixels.length;part.cx/=part.area;part.cy/=part.area;
      part.width=part.maxX-part.minX+1;part.height=part.maxY-part.minY+1;
      parts.push(part);
    }
    return {parts,labels};
  };
  const originals=components(covered), generated=components(clean);
  const used=new Set(), registrations=[];
  let fallbackPixels=0,extendedPixels=0;
  for(const original of [...originals.parts].sort((a,b)=>b.area-a.area)) {
    let match,score=Infinity;
    for(const candidate of generated.parts) {
      if(used.has(candidate.label)||candidate.area<4)continue;
      const ratio=candidate.area/original.area;
      const distance=Math.hypot(candidate.cx-original.cx,candidate.cy-original.cy);
      const reach=Math.max(65,Math.hypot(original.width,original.height)*0.4);
      if(ratio<0.08||ratio>10||distance>reach)continue;
      const value=(distance/reach)**2+Math.log(ratio)**2*0.08;
      if(value<score){score=value;match=candidate;}
    }
    if(!match) {
      for(const p of original.pixels) {
        const sx=Math.floor(p%size/2),sy=Math.floor(Math.floor(p/size)/2);
        const sum=[0,0,0];let weight=0;
        // Soften the old high-contrast outline on tiny unmatched fragments.
        // Sample original covered terrain only; transparent RGB is undefined.
        for(let y=Math.max(0,sy-2);y<=Math.min(originalSize-1,sy+2);y++)for(let x=Math.max(0,sx-2);x<=Math.min(originalSize-1,sx+2);x++) {
          const src=(y*originalSize+x)*4,w=source[src+3]/255;
          for(let c=0;c<3;c++)sum[c]+=source[src+c]*w;
          weight+=w;
        }
        for(let c=0;c<3;c++)output[p*4+c]=Math.round(sum[c]/Math.max(0.001,weight)*0.85);
        fallbackPixels++;
      }
      registrations.push({originalArea:original.area,originalCenter:[original.cx,original.cy],fallback:true});
      continue;
    }
    used.add(match.label);
    // Fill only this matched texture's bounding rectangle from its own nearest
    // clean paint samples, so a neighbouring island can never bleed into it.
    const width=match.width,height=match.height, nearest=new Int32Array(width*height);nearest.fill(-1);
    const localQueue=new Int32Array(width*height);let head=0,tail=0;
    for(const p of match.pixels) {
      const local=(Math.floor(p/size)-match.minY)*width+(p%size-match.minX);
      nearest[local]=p;localQueue[tail++]=local;
    }
    while(head<tail) {
      const p=localQueue[head++],x=p%width;
      const visit=n=>{if(nearest[n]===-1){nearest[n]=nearest[p];localQueue[tail++]=n;}};
      if(x)visit(p-1);if(x+1<width)visit(p+1);if(p>=width)visit(p-width);if(p+width<nearest.length)visit(p+width);
    }
    for(const p of original.pixels) {
      const x=p%size,y=Math.floor(p/size);
      const u=Math.min(width-1,Math.floor((x-original.minX+0.5)/original.width*width));
      const v=Math.min(height-1,Math.floor((y-original.minY+0.5)/original.height*height));
      const q=nearest[v*width+u];
      for(let c=0;c<3;c++)output[p*4+c]=paint[q*4+c];
      if(generated.labels[(v+match.minY)*size+u+match.minX]!==match.label)extendedPixels++;
    }
    registrations.push({originalArea:original.area,paintArea:match.area,originalCenter:[original.cx,original.cy],paintCenter:[match.cx,match.cy],fallback:false});
  }
  return {pixels:output,report:{sourceCoveredPixels,fallbackPixels,extendedPixels,
    fallbackFraction:fallbackPixels/Math.max(1,sourceCoveredPixels),originalComponents:originals.parts.length,
    paintedComponents:registrations.filter(part=>!part.fallback).length,registrations}};
};
