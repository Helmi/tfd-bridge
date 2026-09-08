// Human review sheet; contains the actual baked asset compositions.
const fs=require('node:fs/promises');
const path=require('node:path');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
(async()=>{
  const maps=[['16_OC_bees_to_honey','Bees to Honey'],['20_NE_two_brothers','Two Brothers'],['18_NE_ice_islands','Ice Islands'],['23_Shards','Shards']];
  const browser=await chromium.launch({channel:'chrome',headless:true});
  try {
    const page=await browser.newPage({viewport:{width:1440,height:1536},deviceScaleFactor:1});
    let body='';
    for(const [id,name] of maps) {
      const bytes=await fs.readFile(path.join(__dirname,'previews',id+'.png'));
      body+=`<figure><figcaption>${name}</figcaption><img src="data:image/png;base64,${bytes.toString('base64')}"></figure>`;
    }
    await page.setContent('<!doctype html><meta charset="utf-8"><style>body{margin:0;padding:24px;background:#06151a;color:#d4e5e5;font:20px system-ui;display:grid;grid-template-columns:1fr 1fr;gap:24px}figure{margin:0}figcaption{height:36px}img{width:100%;display:block}</style>'+body);
    await page.evaluate(()=>Promise.all([...document.images].map(image=>image.decode())));
    await page.screenshot({path:path.join(__dirname,'previews','four-maps.jpg'),type:'jpeg',quality:95,fullPage:true});
    for(const [id] of maps) {
      await page.setViewportSize({width:1000,height:1000});
      await page.setContent(`<style>body{margin:0}img{width:1000px;display:block}</style><img src="data:image/png;base64,${(await fs.readFile(path.join(__dirname,'previews',id+'.png'))).toString('base64')}">`);
      await page.locator('img').evaluate(image=>image.decode());
      await page.screenshot({path:path.join(__dirname,'previews',id+'.jpg'),type:'jpeg',quality:96});
    }
  }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1});
