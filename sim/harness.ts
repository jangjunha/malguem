/**
 * Relay-broadcast simulation harness.
 *
 *   sudo node --experimental-strip-types harness.ts [scenario ...]
 *
 * For each scenario: starts the link emulator (netem/emulator.py), which puts
 * every participant in its own network namespace behind a modelled home
 * connection; launches one headless Chromium per participant *inside* its
 * namespace; loads page/main.ts (the real RelayHub over real WebRTC data
 * channels); plays the scenario timeline; and writes raw logs plus a summary
 * to results/<scenario>/.
 *
 * Signaling goes through this process (standing in for malguem-server), with
 * a small fixed delay; only media and relay control cross the emulated links.
 */
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync, appendFileSync, chmodSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, type Browser, type Page } from 'playwright-core';
import { PROFILES, SCENARIOS, type Scenario } from './scenarios.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const CHROME = process.env.CHROME_PATH ?? '/opt/pw-browsers/chromium-1194/chrome-linux/chrome';
const SIGNAL_DELAY_MS = 15;

interface StatRow {
  node: string;
  ts: number; // harness-relative seconds
  [k: string]: unknown;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * CPU used by each simulated participant (all its browser processes: they
 * all live in its network namespace), in % of one core, since the last call.
 */
const nodeCpuPrev = new Map<string, { ticks: number; at: number }>();
function nodeCpu(name: string): number | null {
  let pids: string[] = [];
  try {
    pids = execFileSync('ip', ['netns', 'pids', `mg-${name}`], { encoding: 'utf8' }).split('\n').filter(Boolean);
  } catch {
    return null;
  }
  let ticks = 0;
  for (const pid of pids) {
    try {
      const f = readFileSync(`/proc/${pid}/stat`, 'utf8');
      const rest = f.slice(f.lastIndexOf(')') + 2).split(' ');
      ticks += Number(rest[11]) + Number(rest[12]); // utime + stime
    } catch {
      /* exited */
    }
  }
  const at = performance.now();
  const prev = nodeCpuPrev.get(name);
  nodeCpuPrev.set(name, { ticks, at });
  if (!prev || ticks < prev.ticks) return null;
  const hz = 100; // USER_HZ
  return Math.round(((ticks - prev.ticks) / hz / ((at - prev.at) / 1000)) * 100);
}

/** Host CPU busy fraction since the previous call (all cores), from /proc/stat. */
let cpuPrev: number[] | null = null;
function cpuBusy(): number | null {
  const f = readFileSync('/proc/stat', 'utf8').split('\n')[0]!.trim().split(/\s+/).slice(1).map(Number);
  const prev = cpuPrev;
  cpuPrev = f;
  if (!prev) return null;
  const d = f.map((x, i) => x - prev[i]!);
  const total = d.reduce((a, b) => a + b, 0);
  const idle = d[3]! + (d[4] ?? 0);
  return total > 0 ? Math.round((1 - idle / total) * 100) : null;
}

type Run = Scenario & { runName?: string };

async function runScenario(sc: Run) {
  const cleanup: (() => Promise<unknown> | void)[] = [];
  try {
    return await runScenarioInner(sc, cleanup);
  } finally {
    for (const fn of cleanup.reverse()) await Promise.resolve(fn()).catch(() => {});
  }
}

async function runScenarioInner(sc: Run, cleanup: (() => Promise<unknown> | void)[]) {
  const dir = join(HERE, 'results', sc.runName ?? sc.name);
  mkdirSync(dir, { recursive: true });
  for (const f of ['stats.jsonl', 'emu.jsonl', 'events.jsonl']) writeFileSync(join(dir, f), '');
  writeFileSync(join(dir, 'scenario.json'), JSON.stringify(sc, null, 2));
  const names = Object.keys(sc.nodes);
  const log = (obj: unknown) => appendFileSync(join(dir, 'events.jsonl'), JSON.stringify(obj) + '\n');

  // ---- emulator ----
  const emuCfg = {
    nodes: Object.fromEntries(names.map((n) => [n, { ...PROFILES[sc.nodes[n]!.profile], ...(sc.nodes[n]!.link ?? {}) }])),
    backbone_ms: sc.backbone_ms,
  };
  const cfgPath = join(dir, 'emu-config.json');
  writeFileSync(cfgPath, JSON.stringify(emuCfg, null, 2));
  const emu: ChildProcess = spawn('python3', [join(HERE, 'netem', 'emulator.py'), cfgPath, '--seed', String(sc.seed ?? 7)], {
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  let t0 = performance.now();
  const rel = () => (performance.now() - t0) / 1000;
  const ready = new Promise<void>((resolve) => {
    createInterface({ input: emu.stdout! }).on('line', (line) => {
      const m = JSON.parse(line);
      if (m.event === 'ready') resolve();
      else if (m.event === 'stats') {
        const cpuByNode = Object.fromEntries(names.map((n) => [n, nodeCpu(n)]));
        appendFileSync(join(dir, 'emu.jsonl'), JSON.stringify({ ts: rel(), cpu: cpuBusy(), cpuByNode, nodes: m.nodes }) + '\n');
      }
    });
  });
  const emuCmd = (obj: unknown) => emu.stdin!.write(JSON.stringify(obj) + '\n');
  cleanup.push(() => {
    if (emu.exitCode === null) emu.kill('SIGTERM');
  });
  await ready;

  // ---- browsers, one per namespace ----
  const bundle = readFileSync(join(HERE, 'dist', 'page.js'), 'utf8');
  const browsers = new Map<string, Browser>();
  const pages = new Map<string, Page>();
  const connected = new Set<string>();
  const alive = new Set(names);

  const onOut = (from: string, kind: string, data: any) => {
    switch (kind) {
      case 'signal': {
        const to = pages.get(data.to);
        if (!to || !alive.has(data.to)) return;
        setTimeout(() => {
          to.evaluate(([f, m]) => window.__simSignal(f, m), [from, data.msg] as const).catch(() => {});
        }, SIGNAL_DELAY_MS);
        break;
      }
      case 'pcstate':
        if (data.state === 'connected') connected.add([from, data.peer].sort().join('|'));
        log({ ts: rel(), node: from, kind, ...data });
        break;
      case 'stats':
        appendFileSync(join(dir, 'stats.jsonl'), JSON.stringify({ node: from, ts: rel(), ...data } satisfies StatRow) + '\n');
        break;
      default:
        log({ ts: rel(), node: from, kind, data });
    }
  };

  await Promise.all(
    names.map(async (name) => {
      const wrapper = join(dir, `chrome-${name}.sh`);
      writeFileSync(wrapper, `#!/bin/sh\nexec ip netns exec mg-${name} ${CHROME} "$@"\n`);
      chmodSync(wrapper, 0o755);
      const browser = await chromium.launch({
        executablePath: wrapper,
        headless: true,
        args: [
          '--no-sandbox',
          '--disable-features=WebRtcHideLocalIpsWithMdns',
          '--disable-background-timer-throttling',
          '--disable-renderer-backgrounding',
          '--disable-backgrounding-occluded-windows',
          '--disable-gpu',
        ],
      });
      browsers.set(name, browser);
      cleanup.push(() => browser.close());
      const page = await browser.newPage();
      await page.route('https://sim.local/**', (route) => {
        const url = route.request().url();
        if (url.endsWith('/page.js')) return route.fulfill({ contentType: 'text/javascript', body: bundle });
        return route.fulfill({ contentType: 'text/html', body: '<!doctype html><script type="module" src="/page.js"></script>' });
      });
      page.on('console', (m) => {
        if (m.type() === 'error' || m.type() === 'warning') log({ ts: rel(), node: name, kind: 'console', text: m.text() });
      });
      page.on('pageerror', (e) => log({ ts: rel(), node: name, kind: 'pageerror', text: String(e) }));
      await page.exposeFunction('__out', (kind: string, data: unknown) => onOut(name, kind, data));
      await page.goto('https://sim.local/index.html');
      await page.waitForFunction(() => typeof window.__simInit === 'function');
      pages.set(name, page);
    }),
  );

  for (const name of names) {
    const spec = sc.nodes[name]!;
    await pages.get(name)!.evaluate((c) => window.__simInit(c), {
      id: name,
      peers: names.filter((n) => n !== name),
      broadcaster: sc.broadcaster,
      bitrateKbps: sc.bitrateKbps,
      fps: sc.fps,
      audioKbps: sc.audioKbps,
      capKbps: spec.capKbps,
      reliability: sc.reliability,
      strategy: sc.strategy,
      keyframeIntervalMs: sc.keyframeIntervalMs,
      svc: sc.svc,
      hubLog: true,
      source: sc.transport === 'mesh' ? 'codec' : (sc.source ?? 'synthetic'),
      transport: sc.transport ?? 'relay',
      width: sc.width ?? 1920,
      height: sc.height ?? 1080,
    });
  }
  for (const name of names) await pages.get(name)!.evaluate(() => window.__simConnect());
  const pairs = (names.length * (names.length - 1)) / 2;
  for (let i = 0; i < 200 && connected.size < pairs; i++) await sleep(100);
  if (connected.size < pairs) throw new Error(`only ${connected.size}/${pairs} peer connections came up`);

  // ---- timeline ----
  t0 = performance.now();
  log({ ts: 0, kind: 'start' });
  const call = (node: string, fn: string, arg?: unknown) =>
    pages
      .get(node)!
      .evaluate(([f, a]) => (window as any)[f](a), [fn, arg] as const)
      .catch((e) => log({ ts: rel(), node, kind: 'harness-error', text: String(e) }));

  const timers: ReturnType<typeof setTimeout>[] = [];
  const at = (s: number, fn: () => void) => timers.push(setTimeout(fn, s * 1000));
  const departed = (node: string, delayS: number) => {
    alive.delete(node);
    at(delayS, () => {
      for (const n of alive) void call(n, '__simPeerLeft', node);
    });
  };

  void call(sc.broadcaster, '__simStart');
  for (const name of names) {
    if (name === sc.broadcaster) continue;
    const w = sc.nodes[name]!.watchAt ?? 1;
    at(w, () => {
      log({ ts: rel(), node: name, kind: 'watch' });
      void call(name, '__simWatch');
    });
  }
  for (const ev of sc.events) {
    at(ev.at, () => {
      log({ ts: rel(), kind: 'scenario-event', ev });
      switch (ev.type) {
        case 'link':
          emuCmd({ cmd: 'set', node: ev.node, ...ev.set });
          break;
        case 'unwatch':
          void call(ev.node, '__simUnwatch');
          break;
        case 'busy':
          void call(ev.node, '__simSetBusy', ev.on);
          break;
        case 'leave':
          void call(ev.node, '__simLeave').then(() => browsers.get(ev.node)?.close());
          departed(ev.node, 0.3); // server sees the WebSocket close right away
          break;
        case 'crash':
          emuCmd({ cmd: 'down', node: ev.node });
          void browsers.get(ev.node)?.close();
          departed(ev.node, sc.crashDetectS);
          break;
      }
    });
  }
  await sleep(sc.durationS * 1000 + 1500);
  for (const t of timers) clearTimeout(t);

  // ---- teardown ----
  await Promise.all([...browsers.values()].map((b) => b.close().catch(() => {})));
  emuCmd({ cmd: 'quit' });
  await new Promise((r) => emu.once('exit', r));
  const summary = summarize(sc, dir);
  writeFileSync(join(dir, 'summary.json'), JSON.stringify(summary, null, 2));
  return summary;
}

// ---------------- analysis ----------------

function readJsonl(path: string): any[] {
  return readFileSync(path, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((l) => JSON.parse(l));
}

const median = (xs: number[]) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)]!;
};
const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
const r1 = (x: number | null) => (x === null ? null : Math.round(x * 10) / 10);

export function summarize(sc: Scenario, dir: string) {
  const stats = readJsonl(join(dir, 'stats.jsonl'));
  const emu = readJsonl(join(dir, 'emu.jsonl'));
  const events = readJsonl(join(dir, 'events.jsonl'));
  const warm = 5; // s excluded at the start
  const end = sc.durationS;
  const viewers = Object.keys(sc.nodes).filter((n) => n !== sc.broadcaster);

  const perViewer: Record<string, unknown> = {};
  for (const v of viewers) {
    const watchAt = sc.nodes[v]!.watchAt ?? 1;
    const leaveEv = sc.events.find((e) => (e.type === 'leave' || e.type === 'crash' || e.type === 'unwatch') && e.node === v);
    const until = leaveEv ? leaveEv.at : end;
    const rows = stats.filter((r) => r.node === v && r.ts >= Math.max(warm, watchAt + 3) && r.ts <= until);
    if (!rows.length) continue;
    const fps = rows.map((r) => r.frames as number);
    const lat50 = rows.map((r) => r.latP50).filter((x): x is number => x !== null);
    const lat95 = rows.map((r) => r.latP95).filter((x): x is number => x !== null);
    const full = rows.filter((r) => (r.fpsByTl as number[])[2]! > 0).length / rows.length;
    const last = stats.filter((r) => r.node === v && r.ts <= until).at(-1);
    const inc = last?.hub?.incoming?.[sc.broadcaster];
    const first = events.find((e) => e.node === v && e.kind === 'event' && e.data?.type === 'first-frame');
    perViewer[v] = {
      fpsAvg: r1(mean(fps)),
      fpsMin: Math.min(...fps),
      fullQualityPct: Math.round(full * 100),
      latP50ms: r1(median(lat50)),
      latP95ms: r1(median(lat95)),
      latWorstP95ms: r1(lat95.length ? Math.max(...lat95) : null),
      hops: r1(median(rows.map((r) => r.hopsAvg).filter((x): x is number => x !== null))),
      freezes: rows.reduce((a, r) => a + (r.freezes as number), 0),
      freezeMs: Math.round(rows.reduce((a, r) => a + (r.freezeMs as number), 0)),
      keyframeRequests: inc?.counters?.keyframeRequests ?? null,
      firstFrameMs: first?.data?.sinceWatchMs != null ? Math.round(first.data.sinceWatchMs) : null,
      height: r1(median(rows.map((r) => r.presentedHeight).filter((x): x is number => x != null))),
      parentAtEnd: inc?.parent ?? null,
    };
  }

  const emuRows = emu.filter((r) => r.ts >= warm && r.ts <= end);
  const upload: Record<string, unknown> = {};
  for (const n of Object.keys(sc.nodes)) {
    const ups = emuRows.map((r) => r.nodes[n]?.up_kbps ?? 0);
    const cpus = emuRows.map((r) => r.cpuByNode?.[n]).filter((x) => x != null);
    upload[n] = {
      cpuPct: r1(mean(cpus)),
      avgMbps: r1((mean(ups) ?? 0) / 1000),
      maxMbps: r1(Math.max(...ups) / 1000),
      qdropUp: emuRows.reduce((a, r) => a + (r.nodes[n]?.qdrop_up ?? 0), 0),
      maxUpBacklogMs: Math.max(...emuRows.map((r) => r.nodes[n]?.max_up_backlog_ms ?? 0)),
    };
  }

  const bRows = stats.filter((r) => r.node === sc.broadcaster);
  const lastOut = bRows.at(-1)?.hub?.outgoing;
  // Event impact: for each scenario event, how long each viewer went without video.
  const impact = sc.events.map((ev) => {
    const affected: Record<string, unknown> = {};
    for (const v of viewers) {
      if (v === ev.node) continue;
      const freezes = events.filter(
        (e) => e.node === v && e.kind === 'event' && e.data?.type === 'freeze' && e.ts >= ev.at && e.ts <= ev.at + 15,
      );
      if (freezes.length) {
        affected[v] = { freezes: freezes.length, worstGapMs: Math.round(Math.max(...freezes.map((f) => f.data.gapMs))) };
      }
    }
    return { event: ev, affected };
  });

  const sumV = Object.values(perViewer) as any[];
  return {
    scenario: sc.name,
    description: sc.description,
    overall: {
      viewers: sumV.length,
      fpsAvg: r1(mean(sumV.map((x) => x.fpsAvg))),
      fullQualityPct: Math.round(mean(sumV.map((x) => x.fullQualityPct)) ?? 0),
      latP50ms: r1(median(sumV.map((x) => x.latP50ms).filter((x) => x !== null))),
      latWorstP95ms: r1(Math.max(...sumV.map((x) => x.latWorstP95ms ?? 0))),
      freezeMsTotal: sumV.reduce((a, x) => a + x.freezeMs, 0),
      broadcasterUpMbps: (upload[sc.broadcaster] as any).avgMbps,
      broadcasterCpuPct: (upload[sc.broadcaster] as any).cpuPct,
      maxRelayCpuPct: Math.max(...viewers.map((v) => (upload[v] as any).cpuPct ?? 0)),
      maxRelayUpMbps: Math.max(...viewers.map((v) => (upload[v] as any).avgMbps)),
      hostCpuPct: r1(mean(emuRows.map((r) => r.cpu).filter((x) => x != null))),
      keyframes: lastOut?.keyframes ?? null,
    },
    perViewer,
    upload,
    finalTree: lastOut?.tree ?? null,
    meshSender: bRows.at(-1)?.mesh ?? null,
    capEstimates: lastOut?.capEstimates ?? null,
    impact,
  };
}

// ---------------- main ----------------

async function main() {
  const wanted = process.argv.slice(2);
  if (wanted[0] === '--summarize') {
    for (const name of wanted.slice(1)) {
      const sc = SCENARIOS.find((s) => s.name === name)!;
      const s = summarize(sc, join(HERE, 'results', name));
      writeFileSync(join(HERE, 'results', name, 'summary.json'), JSON.stringify(s, null, 2));
      console.log(JSON.stringify(s.overall));
    }
    return;
  }
  const list = wanted.length ? SCENARIOS.filter((s) => wanted.includes(s.name)) : SCENARIOS;
  const dur = process.env.SIM_DURATION ? Number(process.env.SIM_DURATION) : null;
  const repeat = Number(process.env.SIM_REPEAT ?? 1);
  for (const sc0 of list) {
    for (let r = 1; r <= repeat; r++) {
      const sc: Run = { ...sc0, ...(dur ? { durationS: dur } : {}), seed: 100 + r, runName: repeat > 1 ? `${sc0.name}@${r}` : sc0.name };
      process.stdout.write(`▶ ${sc.runName} … `);
      const s = await runScenario(sc);
      console.log(JSON.stringify(s.overall));
    }
  }
}

void main().catch((e) => {
  console.error(e);
  process.exit(1);
});
