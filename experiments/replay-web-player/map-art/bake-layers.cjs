// Deterministic asset finishing. Artwork comes from image_gen; geometry comes
// exclusively from the original game's alpha, never from generated outlines.
const fs = require('node:fs/promises');
const path = require('node:path');
const crypto = require('node:crypto');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const root = __dirname;
const registerLand = require('./register-land.cjs');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const readJson = async file => JSON.parse((await fs.readFile(file, 'utf8')).replace(/^\uFEFF/, ''));
const bytesFromUrl = url => Buffer.from(url.split(',')[1], 'base64');

(async () => {
  const sources = await readJson(path.join(root, 'sources/manifest.json'));
  const catalog = await readJson(path.join(root, '../web/src/assets/maps/catalog.json'));
  const requested = process.argv.slice(2);
  const maps = catalog.maps.filter(map => !requested.length || requested.includes(map.id.slice(7)));
  const browser = await chromium.launch({channel:'chrome',headless:true});
  try {
    const page = await browser.newPage();
    await page.addScriptTag({content: `window.registerLand = ${registerLand.toString()}`});
    for (const map of maps) {
      const id = map.id.slice(7);
      const source = sources.maps.find(entry => entry.id === map.id);
      if (!source || source.sceneCompositeSha256 !== map.sourceCompositeSha256) throw new Error(`Source mismatch: ${id}`);
      const generated = path.join(root, 'generated', id);
      const originalBytes = await fs.readFile(path.join(root, 'sources', source.land.path));
      if (sha(originalBytes) !== source.land.sha256) throw new Error(`Original land changed: ${id}`);
      const landBytes = await fs.readFile(path.join(generated, 'land.png'));
      const waterBytes = await fs.readFile(path.join(generated, 'water.png'));
      const dataUrl = bytes => `data:image/png;base64,${bytes.toString('base64')}`;
      const result = await page.evaluate(async ({originalUrl,landUrl,waterUrl,size}) => {
        const image = async url => createImageBitmap(await(await fetch(url)).blob(), {premultiplyAlpha:'none'});
        const [original,paint,water] = await Promise.all([image(originalUrl),image(landUrl),image(waterUrl)]);
        if (original.width !== 760 || original.height !== 760 || size !== 1520) throw new Error('Unexpected original mask dimensions');
        if (paint.width !== paint.height || water.width !== water.height) throw new Error('Generated artwork is not square');
        const canvas = (width,height) => {const c=document.createElement('canvas');c.width=width;c.height=height;return c;};
        const sourceCanvas = canvas(760,760);
        const sourceContext = sourceCanvas.getContext('2d',{willReadFrequently:true});
        sourceContext.drawImage(original,0,0);
        const sourcePixels = sourceContext.getImageData(0,0,760,760).data;
        const landCanvas = canvas(size,size), waterCanvas = canvas(size,size);
        const ctx = landCanvas.getContext('2d',{willReadFrequently:true});
        ctx.imageSmoothingQuality='high';ctx.drawImage(paint,0,0,size,size);
        const paintPixels = ctx.getImageData(0,0,size,size).data;
        const finished = ctx.createImageData(size,size), count=size*size;
        const registered = window.registerLand(sourcePixels,paintPixels,size);
        finished.data.set(registered.pixels);
        ctx.putImageData(finished,0,0);
        const waterContext=waterCanvas.getContext('2d');
        waterContext.fillStyle='#06172a';waterContext.fillRect(0,0,size,size);
        waterContext.imageSmoothingQuality='high';waterContext.drawImage(water,0,0,size,size);
        const landOutput=landCanvas.toDataURL('image/webp',0.96);
        const waterOutput=waterCanvas.toDataURL('image/webp',0.92);
        const decoded=await image(landOutput),decodedWater=await image(waterOutput);
        ctx.clearRect(0,0,size,size);ctx.drawImage(decoded,0,0);
        const encodedPixels=ctx.getImageData(0,0,size,size).data;
        let alphaMismatches=0,keyContaminationPixels=0;
        for(let p=0;p<count;p++) {
          const src=(Math.floor(Math.floor(p/size)/2)*760+Math.floor((p%size)/2))*4;
          if(encodedPixels[p*4+3]!==sourcePixels[src+3])alphaMismatches++;
          const r=encodedPixels[p*4],g=encodedPixels[p*4+1],b=encodedPixels[p*4+2];
          if(encodedPixels[p*4+3]>200 && r>g+25 && b>g+25)keyContaminationPixels++;
        }
        if(alphaMismatches)throw new Error(`WebP changed ${alphaMismatches} original alpha samples`);
        if(keyContaminationPixels)throw new Error(`Key fringe remains in ${keyContaminationPixels} encoded terrain samples`);
        const preview=canvas(size,size),previewContext=preview.getContext('2d');
        previewContext.drawImage(decodedWater,0,0);previewContext.drawImage(decoded,0,0);
        const report={width:size,height:size,generatedLandSize:[paint.width,paint.height],generatedWaterSize:[water.width,water.height],
          alphaMismatches,keyContaminationPixels,...registered.report};
        original.close();paint.close();water.close();decoded.close();decodedWater.close();
        return {land:landOutput,water:waterOutput,preview:preview.toDataURL('image/png'),report};
      },{originalUrl:dataUrl(originalBytes),landUrl:dataUrl(landBytes),waterUrl:dataUrl(waterBytes),size:map.width});
      const landOutput=bytesFromUrl(result.land),waterOutput=bytesFromUrl(result.water);
      const publicDir=path.join(root,'../web/public');
      await fs.mkdir(path.dirname(path.join(publicDir,map.land)),{recursive:true});
      await fs.writeFile(path.join(publicDir,map.land),landOutput);
      await fs.writeFile(path.join(publicDir,map.water),waterOutput);
      const previewDir=path.join(root,'previews');await fs.mkdir(previewDir,{recursive:true});
      await fs.writeFile(path.join(previewDir,id+'.png'),bytesFromUrl(result.preview));
      const report={mapId:map.id,recipeVersion:2,sourceLandSha256:sha(originalBytes),generatedLandSha256:sha(landBytes),generatedWaterSha256:sha(waterBytes),
        landSha256:sha(landOutput),waterSha256:sha(waterOutput),landBytes:landOutput.length,waterBytes:waterOutput.length,...result.report};
      await fs.mkdir(path.join(root,'verification'),{recursive:true});
      await fs.writeFile(path.join(root,'verification',id+'.json'),JSON.stringify(report,null,2)+'\n');
      const {registrations,...summary}=report;
      console.log(JSON.stringify(summary));
    }
  }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1});
