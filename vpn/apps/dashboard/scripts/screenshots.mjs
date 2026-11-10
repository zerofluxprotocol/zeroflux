// Loads every dashboard page in headless Chromium, saves full-page screenshots
// to screenshots/, and fails if any page logs a console error, throws, or gets
// a failed API response. Nearly read-only: it opens dialogs and cancels them,
// walks the onboarding wizard without pressing Finish, and never approves
// anything. The only writes are a temporary custom entity type (added through
// the UI, then removed through the UI) and detection-model test runs.
//
//   PV_DAEMON_URL=http://127.0.0.1:8799 \
//   PV_ADMIN_TOKEN_FILE=/tmp/pv-e2e-home/admin.token \
//   PV_DASH_URL=http://127.0.0.1:5173 npm run screenshots
//
// PV_DASH_URL defaults to the daemon itself (the embedded build).
import { chromium } from 'playwright-core';
import { mkdirSync, readFileSync, existsSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, '../screenshots');
mkdirSync(out, { recursive: true });

const daemon = process.env.PV_DAEMON_URL ?? 'http://127.0.0.1:8787';
const dash = process.env.PV_DASH_URL ?? daemon;
const token = process.env.PV_ADMIN_TOKEN ?? readFileSync(process.env.PV_ADMIN_TOKEN_FILE ?? join(homedir(), '.promptvpn/admin.token'), 'utf8').trim();

/** Use an already-downloaded Chromium if Playwright's pinned build is absent. */
function findChromium() {
  if (process.env.CHROMIUM_PATH) return process.env.CHROMIUM_PATH;
  const cache = process.platform === 'darwin' ? join(homedir(), 'Library/Caches/ms-playwright') : join(homedir(), '.cache/ms-playwright');
  if (!existsSync(cache)) return undefined;
  const dirs = readdirSync(cache).filter((d) => d.startsWith('chromium_headless_shell-')).sort().reverse();
  for (const d of dirs) {
    for (const rel of ['chrome-headless-shell-mac-arm64/chrome-headless-shell', 'chrome-mac/headless_shell', 'chrome-headless-shell-mac-x64/chrome-headless-shell', 'chrome-headless-shell-linux64/chrome-headless-shell', 'chrome-linux/headless_shell']) {
      const p = join(cache, d, rel);
      if (existsSync(p)) return p;
    }
  }
  return undefined;
}

async function adminGet(path) {
  const r = await fetch(daemon + '/api/admin/v1' + path, { headers: { authorization: `Bearer ${token}` } });
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return r.json();
}

const ticket = (await (await fetch(daemon + '/api/admin/v1/session/ticket', { method: 'POST', headers: { authorization: `Bearer ${token}` } })).json()).ticket;

let browser;
try {
  browser = await chromium.launch();
} catch {
  browser = await chromium.launch({ executablePath: findChromium() });
}
const ctx = await browser.newContext({ viewport: { width: 1360, height: 900 }, deviceScaleFactor: 2 });
const page = await ctx.newPage();
const problems = [];
let current = 'login';
page.on('console', (m) => m.type() === 'error' && problems.push(`[${current}] console: ${m.text()}`));
page.on('pageerror', (e) => problems.push(`[${current}] pageerror: ${e.message}`));
page.on('response', (r) => {
  if (r.url().includes('/api/') && r.status() >= 400) problems.push(`[${current}] ${r.status()} ${r.request().method()} ${r.url()}`);
});

await page.goto(`${dash}/dashboard/login?ticket=${ticket}`);
await page.waitForURL(/\/dashboard\/(#.*)?$/);

async function shot(name, route, after) {
  current = name;
  if (route !== null) {
    await page.goto(`${dash}/dashboard/#${route}`);
  }
  await page.waitForLoadState('networkidle');
  await page.waitForTimeout(400);
  if (after) await after();
  await page.waitForTimeout(250);
  await page.screenshot({ path: join(out, `${name}.png`), fullPage: true });
  console.log('saved', name);
}

const events = (await adminGet('/events?limit=50')).events;
const firstRequest = events.find((e) => e.kind === 'request' && e.action !== 'ALLOW') ?? events[0];
const approvals = (await adminGet('/approvals?all=true')).approvals;
const pending = approvals.find((a) => a.status === 'PENDING' && new Date(a.expires_at) > new Date());
const anyApproval = pending ?? approvals[0];

await shot('01-overview', '/overview');
await shot('02-activity', '/activity');
await shot('03-activity-filtered-block', '/activity', async () => {
  await page.getByRole('combobox').first().selectOption('BLOCK');
  await page.waitForLoadState('networkidle');
});
if (firstRequest) await shot('04-event-detail', '/activity/' + encodeURIComponent(firstRequest.event_id));
await shot('05-approvals', '/approvals');
if (anyApproval) await shot('06-review-required', '/approvals/' + encodeURIComponent(anyApproval.approval_id));
if (pending) {
  await shot('07-review-reveal-confirm', null, async () => {
    await page.getByRole('button', { name: 'Reveal original' }).first().click();
  });
  await page.getByRole('button', { name: 'Cancel' }).click();
  await shot('08-review-save-as-rule', null, async () => {
    await page.getByRole('button', { name: 'Save as rule' }).click();
  });
  await shot('08b-review-save-as-entity', null, async () => {
    await page.getByRole('radio', { name: 'Custom entity type' }).click();
    await page.getByPlaceholder('insurance policy number').fill('employee badge number');
  });
  await page.getByRole('button', { name: 'Cancel' }).click();
}
await shot('09-policies', '/policies');
await shot('10-policies-playground', null, async () => {
  await page.getByRole('button', { name: 'Test', exact: true }).click();
  await page.waitForLoadState('networkidle');
  await page.getByText('Decision', { exact: true }).waitFor();
});
await shot('11-policy-new-rule', null, async () => {
  await page.getByRole('button', { name: 'New rule' }).click();
});
await page.getByRole('button', { name: 'Cancel' }).click();
await shot('12-categories', '/categories');
await shot('13-categories-disable-critical', null, async () => {
  await page.getByRole('switch', { name: 'API keys protection' }).click();
  await page.getByLabel('Confirmation phrase').fill('DISABLE API_KEY');
});
await page.getByRole('button', { name: 'Keep protection on' }).click();
await shot('14-secrets', '/secrets');
const TEMP_ENTITY = 'screenshot badge number';
const hadTemp = (await adminGet('/entities')).entities.some((e) => e.entity.label === TEMP_ENTITY);
await shot('15-entities', '/entities');
if (!hadTemp) {
  await shot('15b-entities-added', null, async () => {
    await page.getByPlaceholder('insurance policy number').fill(TEMP_ENTITY);
    await page.getByRole('button', { name: 'Add entity type' }).click();
    await page.getByText(`Added "${TEMP_ENTITY}"`).waitFor();
  });
  await shot('15c-entities-remove-confirm', null, async () => {
    await page.locator('.list-item', { hasText: TEMP_ENTITY }).getByRole('button', { name: 'Remove' }).click();
  });
  await page.getByRole('dialog').getByRole('button', { name: 'Remove' }).click();
  await page.getByText(`Removed "${TEMP_ENTITY}"`).waitFor();
  if ((await adminGet('/entities')).entities.some((e) => e.entity.label === TEMP_ENTITY)) problems.push('[entities] temporary entity was not removed');
}
await shot('15d-entities-builtin-label', '/entities', async () => {
  await page.getByPlaceholder('insurance policy number').fill('Email address');
  await page.getByText('already covered by the built-in category').waitFor();
});
await shot('16-apps', '/apps');
await shot('17-models', '/models');
await shot('17b-models-test-detection', null, async () => {
  await page.getByRole('button', { name: 'Test detection' }).click();
  await page.getByText(/spans? found in/).waitFor({ timeout: 30_000 });
});
await shot('18-settings', '/settings');
await shot('19-health', '/health');
for (let step = 1; step <= 6; step++) {
  await shot(`2${step}-onboarding-step-${step}`, step === 1 ? '/onboarding' : null, async () => {
    if (step === 3) {
      await page.getByRole('switch', { name: 'Add custom entity types' }).click();
      await page.getByPlaceholder('insurance policy number').fill('claim reference');
      await page.getByRole('button', { name: 'Add entity type' }).click();
    }
    if (step === 5) await page.getByText('Detection model', { exact: true }).waitFor();
    if (step === 5) await page.getByText('Verified model').waitFor();
    if (step === 5) await page.waitForTimeout(4000); // attestation check
  });
  if (step === 5) await page.getByRole('button', { name: 'Skip for now' }).click();
  else if (step < 6) await page.getByRole('button', { name: 'Continue' }).click();
}

await browser.close();
if (problems.length) {
  console.error('\nProblems:\n' + problems.join('\n'));
  process.exit(1);
}
console.log(`\nAll pages loaded without console errors. Screenshots in ${out}`);
