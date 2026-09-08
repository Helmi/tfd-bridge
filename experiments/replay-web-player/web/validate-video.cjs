// Run with PLAYWRIGHT_MODULE pointing at an installed Playwright package.
// Writes the actual browser-produced MP4 and a decoded frame to the output dir.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const fs = require('node:fs/promises');
const path = require('node:path');

(async () => {
  const output = path.resolve(process.argv[2]);
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1920, height: 1200 }, acceptDownloads: true });
    page.on('pageerror', error => console.error('PAGE ERROR:', error.message));
    page.on('requestfailed', request => console.error('REQUEST FAILED:', request.url().split('?')[0], request.failure()?.errorText));
    await page.goto('http://127.0.0.1:5178/video-check.html');
    await page.getByRole('button', { name: process.argv[3] === '--budget' ? 'Encode full battle under 20 MiB' : 'Encode full battle', exact: true }).click();
    let status = '';
    const deadline = Date.now() + 30 * 60 * 1000;
    while (Date.now() < deadline) {
      status = await page.locator('#status').innerText();
      console.log(new Date().toISOString(), status);
      if (status.startsWith('Encoded ')) break;
      if (/error|failed/i.test(status)) throw new Error(status);
      await page.waitForTimeout(20000);
    }
    if (!status.startsWith('Encoded ')) throw new Error('Render did not finish within 30 minutes');
    const downloadReady = page.waitForEvent('download');
    await page.getByRole('link', { name: 'Save validation MP4' }).click();
    const download = await downloadReady;
    const file = path.join(output, 'full-battle.mp4');
    await download.saveAs(file);
    const metadata = await page.locator('video').evaluate(async video => {
      if (video.readyState < 1) await new Promise(resolve => video.addEventListener('loadedmetadata', resolve, {once:true}));
      const metadata = {duration:video.duration,width:video.videoWidth,height:video.videoHeight};
      video.currentTime = Math.min(64, video.duration - 0.1);
      await new Promise(resolve => video.addEventListener('seeked', resolve, {once:true}));
      return metadata;
    });
    await page.locator('video').screenshot({ path: path.join(output, 'decoded-frame.png') });
    const report = {status, ...metadata, bytes:(await fs.stat(file)).size, file};
    await fs.writeFile(path.join(output,'result.json'), JSON.stringify(report,null,2));
    console.log(JSON.stringify(report));
  } finally {await browser.close();}
})().catch(error => {console.error(error);process.exitCode=1;});
