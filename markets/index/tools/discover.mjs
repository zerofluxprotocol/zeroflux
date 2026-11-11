// Endpoint discovery: load OpenRouter pages in headless Chrome and record every
// request/response (URL, method, status, headers, body) to a JSONL file.
// Usage: node discover.mjs <out_dir> <url> [<url> ...]
// Requires playwright-core (PW_PATH env, default ~/zeroflux-demo/pw/node_modules/playwright-core).
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
const pwPath = process.env.PW_PATH || path.join(os.homedir(), 'zeroflux-demo/pw/node_modules/playwright-core/index.js');
const pw = await import(pwPath); const chromium = pw.chromium || pw.default.chromium;
const [outDir, ...urls] = process.argv.slice(2);
fs.mkdirSync(path.join(outDir, 'bodies'), { recursive: true });
const log = fs.createWriteStream(path.join(outDir, 'requests.jsonl'), { flags: 'a' });
const browser = await chromium.launch({
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
});
const ctx = await browser.newContext({ viewport: { width: 1400, height: 1000 } });
const page = await ctx.newPage();
let n = 0;
page.on('response', async (resp) => {
  const req = resp.request();
  const type = req.resourceType();
  if (['image', 'font', 'stylesheet', 'media'].includes(type)) return;
  const rec = {
    page: page.url(), url: req.url(), method: req.method(), type,
    status: resp.status(), req_headers: req.headers(), resp_headers: resp.headers(),
    post_data: req.postData() || null, t: new Date().toISOString(),
  };
  try {
    const body = await resp.body();
    const id = String(++n).padStart(5, '0');
    fs.writeFileSync(path.join(outDir, 'bodies', id), body);
    rec.body_file = id; rec.body_len = body.length;
  } catch (e) { rec.body_err = String(e); }
  log.write(JSON.stringify(rec) + '\n');
});
for (const u of urls) {
  console.error('loading', u);
  try {
    await page.goto(u, { waitUntil: 'networkidle', timeout: 60000 });
  } catch (e) { console.error('goto', u, String(e)); }
  await page.waitForTimeout(3000 + Math.floor(Math.random() * 3000));
  // scroll to trigger lazy charts
  for (let i = 0; i < 6; i++) { await page.mouse.wheel(0, 1200); await page.waitForTimeout(800); }
  await page.waitForTimeout(2000);
}
await browser.close();
log.end();
