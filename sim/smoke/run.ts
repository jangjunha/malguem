/**
 * App-integration smoke test: three CallManagers (one sharing in relay mode,
 * two watching) in three headless Chromiums; this process plays the server
 * (roster + signed-signal forwarding). Passes if both viewers render frames
 * of the relayed screen and one of them receives it through the other.
 *
 *   node --experimental-strip-types smoke/run.ts
 */
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, type Page } from 'playwright-core';

const HERE = dirname(fileURLToPath(import.meta.url));
const CHROME = process.env.CHROME_PATH ?? '/opt/pw-browsers/chromium-1194/chrome-linux/chrome';
const bundle = readFileSync(join(HERE, '..', 'dist', 'smoke.js'), 'utf8');
const ids = ['alice', 'bob', 'carol'];
const pages = new Map<string, Page>();
const roster = new Set<string>();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const deliver = (to: string, ev: unknown) => void pages.get(to)?.evaluate((e) => window.__event(e as never), ev).catch(() => {});

async function main() {
  const browsers = [];
  for (const id of ids) {
    const b = await chromium.launch({
      executablePath: CHROME,
      headless: true,
      args: ['--no-sandbox', '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--disable-gpu'],
    });
    browsers.push(b);
    const page = await b.newPage();
    page.on('pageerror', (e) => console.log(`[${id}] pageerror`, e.message));
    page.on('console', (m) => {
      if (m.type() === 'error' || m.type() === 'warning') console.log(`[${id}] ${m.text()}`);
    });
    await page.route('https://smoke.local/**', (r) =>
      r.request().url().endsWith('.js')
        ? r.fulfill({ contentType: 'text/javascript', body: bundle })
        : r.fulfill({ contentType: 'text/html', body: '<!doctype html><script type="module" src="/smoke.js"></script>' }),
    );
    await page.exposeFunction('__out', (kind: string, ev: any) => {
      if (kind !== 'send') return;
      if (ev.type === 'call_join') {
        roster.add(id);
        for (const p of roster) deliver(p, { type: 'call_roster', channel_id: ev.channel_id, participants: [...roster] });
      } else if (ev.type === 'signal') {
        deliver(ev.to, { type: 'signal', channel_id: ev.channel_id, from: id, payload: ev.payload, sig: ev.sig });
      }
    });
    await page.goto('https://smoke.local/');
    await page.waitForFunction(() => typeof window.__init === 'function');
    pages.set(id, page);
  }
  const pubs: Record<string, string> = {};
  for (const id of ids) pubs[id] = await pages.get(id)!.evaluate((i) => window.__init(i), id);
  for (const id of ids) await pages.get(id)!.evaluate((p) => window.__setPubs(p), pubs);
  for (const id of ids) await pages.get(id)!.evaluate(() => window.__join());
  await sleep(3000);
  await pages.get('alice')!.evaluate(() => window.__share());
  await sleep(10000);
  let ok = true;
  const parents: string[] = [];
  for (const id of ids) {
    const st: any = await pages.get(id)!.evaluate(() => window.__status());
    const inc = st.relay?.incoming?.alice;
    console.log(id, 'frames from alice:', st.frames.alice ?? 0, '| relay parent:', inc?.parent ?? '-', '| rx fps:', inc?.rxFps ?? '-', st.relayBroadcasting ? '| broadcasting via relay' : '');
    if (id !== 'alice' && !((st.frames.alice ?? 0) > 100)) ok = false;
    if (id !== 'alice') parents.push(inc?.parent);
    if (id === 'alice' && !st.relayBroadcasting) ok = false;
  }
  // One viewer must be fed through the other (a real relay hop).
  if (!parents.some((p) => p === 'bob' || p === 'carol')) ok = false;
  for (const b of browsers) await b.close();
  console.log(ok ? 'SMOKE OK' : 'SMOKE FAILED');
  process.exit(ok ? 0 : 1);
}
void main();
