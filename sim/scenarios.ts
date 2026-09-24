/**
 * Environment constraints and scenarios for the relay-broadcast simulation.
 *
 * Profiles model the home connections of a Korean friend group. Numbers are
 * *effective* rates a desktop actually gets (not the plan's headline figure),
 * chosen conservatively:
 *
 * - "기가" FTTH (KT/SKB/LGU+ 1 Gbps plans): symmetric; wired desktops
 *   typically see 500–900 Mbps. We use 500 Mbps.
 * - 500M / 100M FTTH plans (광랜, apartment LAN): symmetric, ~90 % of plan.
 * - Older apartments on VDSL: 100 Mbps plan with ~30–50 Mbps real upload,
 *   and a larger modem buffer.
 * - Desktop on Wi-Fi far from the AP (another room, 2.4 GHz): effective
 *   60/40 Mbps, bursty extra delay, ~0.5–1 % loss in short bursts.
 * - Phone tethering (5G/LTE hotspot): asymmetric, higher base latency, jitter.
 * - Home routers have deep uplink buffers (bufferbloat): 100–300 ms of queue
 *   before tail drop.
 *
 * Latency: domestic Korean RTTs are small. Access hop 1–4 ms (FTTH),
 * metro backbone ~1 ms within Seoul/capital area, ~4 ms Seoul↔Busan one-way.
 *
 * `capKbps` is what the participant *declares* as upload to spare for relaying
 * (a client setting; a sensible default would be "a quarter of measured
 * upload"). Several scenarios deliberately make it wrong.
 */

export interface LinkProfile {
  up_kbps: number;
  down_kbps: number;
  access_ms: number;
  jitter_ms: number;
  loss: number;
  loss_burst: number;
  queue_ms: number;
  region: string;
}

export const PROFILES = {
  giga: { up_kbps: 500_000, down_kbps: 500_000, access_ms: 2, jitter_ms: 0, loss: 0, loss_burst: 1, queue_ms: 100, region: 'seoul' },
  ftth500: { up_kbps: 450_000, down_kbps: 450_000, access_ms: 2, jitter_ms: 0, loss: 0, loss_burst: 1, queue_ms: 100, region: 'seoul' },
  ftth100: { up_kbps: 90_000, down_kbps: 90_000, access_ms: 3, jitter_ms: 0, loss: 0, loss_burst: 1, queue_ms: 150, region: 'seoul' },
  vdsl: { up_kbps: 35_000, down_kbps: 90_000, access_ms: 5, jitter_ms: 0.5, loss: 0.0005, loss_burst: 2, queue_ms: 250, region: 'seoul' },
  wifiWeak: { up_kbps: 40_000, down_kbps: 60_000, access_ms: 4, jitter_ms: 3, loss: 0.008, loss_burst: 3, queue_ms: 120, region: 'seoul' },
  tether: { up_kbps: 12_000, down_kbps: 60_000, access_ms: 15, jitter_ms: 5, loss: 0.003, loss_burst: 2, queue_ms: 200, region: 'seoul' },
  // A streamer's uplink that the game, Discord-era habits and a cloud backup
  // already eat into: only ~20 Mbps left for the broadcast.
  busyUplink: { up_kbps: 20_000, down_kbps: 300_000, access_ms: 2, jitter_ms: 0, loss: 0, loss_burst: 1, queue_ms: 200, region: 'seoul' },
} satisfies Record<string, LinkProfile>;

export type ProfileName = keyof typeof PROFILES;

export interface NodeSpec {
  profile: ProfileName;
  /** Overrides on top of the profile (e.g. region). */
  link?: Partial<LinkProfile>;
  /** Declared relay capacity, kbps. */
  capKbps: number;
  /** When this viewer starts watching, s (default 1). */
  watchAt?: number;
}

export type TimelineEvent =
  | { at: number; type: 'link'; node: string; set: Partial<LinkProfile> }
  | { at: number; type: 'leave'; node: string } // graceful: closes connections
  | { at: number; type: 'crash'; node: string } // blackhole, roster notices later
  | { at: number; type: 'unwatch'; node: string };

export interface Scenario {
  name: string;
  description: string;
  broadcaster: string;
  nodes: Record<string, NodeSpec>;
  durationS: number;
  bitrateKbps: number;
  fps: number;
  strategy: 'tree' | 'star';
  reliability:
    | { kind: 'split'; enhLifetimeMs: number }
    | { kind: 'partial'; maxPacketLifeTime: number }
    | { kind: 'reliable' };
  svc: 'L1T3' | 'L1T1';
  keyframeIntervalMs: number;
  audioKbps: number;
  events: TimelineEvent[];
  /** How long the server roster takes to drop a crashed client, s. */
  crashDetectS: number;
  backbone_ms: Record<string, number>;
  /** Emulator RNG seed (loss/jitter draws). */
  seed?: number;
  source?: 'synthetic' | 'codec';
  /** 'mesh' = today's app (WebRTC video track per viewer); default 'relay'. */
  transport?: 'relay' | 'mesh';
  width?: number;
  height?: number;
}

const BACKBONE = { seoul: 1, busan: 1, 'seoul-busan': 4, default: 3 };

/** The friend group used across scenarios: 1 streamer + 7 viewers. */
function group(broadcasterProfile: ProfileName, broadcasterCap: number): Record<string, NodeSpec> {
  return {
    streamer: { profile: broadcasterProfile, capKbps: broadcasterCap },
    minji: { profile: 'giga', capKbps: 100_000 },
    junho: { profile: 'ftth500', capKbps: 60_000 },
    seoyeon: { profile: 'ftth100', capKbps: 25_000, link: { region: 'busan' } },
    dohyun: { profile: 'ftth100', capKbps: 25_000 },
    haeun: { profile: 'vdsl', capKbps: 10_000 },
    jiwoo: { profile: 'wifiWeak', capKbps: 8_000 },
    taeyang: { profile: 'tether', capKbps: 0 },
  };
}

const base = {
  broadcaster: 'streamer',
  durationS: 40,
  bitrateKbps: 8000,
  fps: 60,
  strategy: 'tree' as const,
  reliability: { kind: 'split', enhLifetimeMs: 300 } as Scenario['reliability'],
  svc: 'L1T3' as const,
  keyframeIntervalMs: 0,
  audioKbps: 96,
  events: [] as TimelineEvent[],
  crashDetectS: 10,
  backbone_ms: BACKBONE,
};

export const SCENARIOS: Scenario[] = [
  {
    ...base,
    name: 'star-busy',
    description: 'Baseline: today\'s behaviour (everyone fed directly by the streamer), streamer uplink ~20 Mbps spare, 7 viewers at 8 Mbps.',
    nodes: group('busyUplink', 20_000),
    strategy: 'star',
  },
  {
    ...base,
    name: 'tree-busy',
    description: 'Same network as star-busy, relay tree enabled.',
    nodes: group('busyUplink', 20_000),
  },
  {
    ...base,
    name: 'tree-busy-reliable',
    description: 'tree-busy with one fully reliable, ordered media channel (head-of-line blocking).',
    nodes: group('busyUplink', 20_000),
    reliability: { kind: 'reliable' },
  },
  {
    ...base,
    name: 'tree-busy-partial',
    description: 'tree-busy with one unordered channel, 400 ms lifetime for every layer (no reliable base layer).',
    nodes: group('busyUplink', 20_000),
    reliability: { kind: 'partial', maxPacketLifeTime: 400 },
  },
  {
    ...base,
    name: 'tree-relay-crash',
    description: 'tree-busy; at t=20 s the busiest relay (minji) crashes (power/network loss, no goodbye). Roster notices only after 10 s.',
    nodes: group('busyUplink', 20_000),
    events: [{ at: 20, type: 'crash', node: 'minji' }],
  },
  {
    ...base,
    name: 'tree-relay-leave',
    description: 'tree-busy; at t=20 s relay minji leaves the call normally.',
    nodes: group('busyUplink', 20_000),
    events: [{ at: 20, type: 'leave', node: 'minji' }],
  },
  {
    ...base,
    name: 'tree-overclaim',
    description: 'junho declares 60 Mbps spare but his real uplink is 12 Mbps (someone at home is uploading). The tree must discover it.',
    nodes: {
      ...group('busyUplink', 20_000),
      junho: { profile: 'ftth500', capKbps: 60_000, link: { up_kbps: 12_000, queue_ms: 250 } },
      minji: { profile: 'giga', capKbps: 16_000 },
    },
  },
  {
    ...base,
    name: 'tree-uplink-drop',
    description: 'tree-busy; at t=15 s relay minji\'s uplink collapses to 10 Mbps (cloud backup starts).',
    nodes: group('busyUplink', 20_000),
    events: [{ at: 15, type: 'link', node: 'minji', set: { up_kbps: 10_000 } }],
  },
  {
    ...base,
    name: 'tree-churn',
    description: 'Viewers come and go: late joins, an unwatch, a leave — checks time-to-first-frame and tree stability.',
    nodes: {
      ...group('busyUplink', 20_000),
      dohyun: { profile: 'ftth100', capKbps: 25_000, watchAt: 12 },
      haeun: { profile: 'vdsl', capKbps: 10_000, watchAt: 18 },
    },
    events: [
      { at: 24, type: 'unwatch', node: 'seoyeon' },
      { at: 30, type: 'leave', node: 'junho' },
    ],
  },
  {
    ...base,
    name: 'tree-lossy-all',
    description: 'Stress: every viewer on lossy Wi-Fi (0.8 % bursty loss, jitter). Tests relay error propagation + keyframe storms.',
    nodes: Object.fromEntries(
      Object.entries(group('busyUplink', 20_000)).map(([k, v]) =>
        k === 'streamer' ? [k, v] : [k, { ...v, link: { ...(v.link ?? {}), jitter_ms: 3, loss: 0.008, loss_burst: 3 } }],
      ),
    ),
  },
  {
    ...base,
    name: 'star-lossy-all',
    description: 'tree-lossy-all network, everyone fed directly by the streamer (for comparison).',
    nodes: Object.fromEntries(
      Object.entries(group('busyUplink', 20_000)).map(([k, v]) =>
        k === 'streamer' ? [k, v] : [k, { ...v, link: { ...(v.link ?? {}), jitter_ms: 3, loss: 0.008, loss_burst: 3 } }],
      ),
    ),
    strategy: 'star',
  },
];

/**
 * Real codec path: WebCodecs L1T3 encode of a canvas animation, decoded by
 * every viewer, through relays, with layer shedding and a relay leaving.
 * Checks that what the relay forwards is actually decodable (decode errors
 * must stay 0). Software codecs in the headless browser, so 720p30.
 */
SCENARIOS.push({
  ...base,
  name: 'codec-tree',
  description: 'Real WebCodecs (software VP9/VP8 L1T3, 720p30, 3 Mbps) through a relay tree; streamer can feed one viewer; a relay leaves at 15 s.',
  durationS: 30,
  bitrateKbps: 3000,
  fps: 30,
  source: 'codec',
  width: 1280,
  height: 720,
  nodes: {
    streamer: { profile: 'busyUplink', capKbps: 5_000 },
    minji: { profile: 'giga', capKbps: 100_000 },
    junho: { profile: 'ftth500', capKbps: 60_000 },
    jiwoo: { profile: 'wifiWeak', capKbps: 0 },
    taeyang: { profile: 'tether', capKbps: 0 },
  },
  events: [{ at: 15, type: 'leave', node: 'minji' }],
});

/**
 * Head-to-head with today's app: a real WebRTC mesh (one video encoder and
 * one congestion controller per viewer) vs. the relay tree, same content and
 * the same 360p30 source, 2/4/6 viewers on good connections, streamer uplink
 * with room to spare. Measures what the streamer's machine pays as the
 * audience grows, and glass-to-glass latency at the viewers. 360p keeps the
 * 8 software codecs from saturating the simulation host, which would distort
 * everything else.
 */
const h2hViewers: [string, NodeSpec][] = [
  ['minji', { profile: 'giga', capKbps: 100_000 }],
  ['junho', { profile: 'ftth500', capKbps: 60_000 }],
  ['seoyeon', { profile: 'ftth100', capKbps: 25_000, link: { region: 'busan' } }],
  ['dohyun', { profile: 'ftth100', capKbps: 25_000 }],
  ['haeun', { profile: 'ftth100', capKbps: 25_000 }],
  ['sujin', { profile: 'ftth500', capKbps: 25_000 }],
];
for (const n of [2, 4, 6]) {
  for (const transport of ['mesh', 'relay'] as const) {
    SCENARIOS.push({
      ...base,
      name: `h2h-${transport}-${n}`,
      description: `${transport === 'mesh' ? "Today's WebRTC mesh" : 'Relay tree'}, ${n} viewers, 360p30 at 1.5 Mb/s.`,
      durationS: 30,
      bitrateKbps: 1500,
      fps: 30,
      source: 'codec',
      transport,
      width: 640,
      height: 360,
      audioKbps: 0,
      nodes: {
        streamer: { profile: 'ftth100', capKbps: 3_000 }, // relay: feeds 1–2 directly
        ...Object.fromEntries(h2hViewers.slice(0, n)),
      },
    });
  }
}

/**
 * Loss sweep: one viewer, fed directly, over a path with random loss. Measures
 * how much residual (post-Wi-Fi-ARQ) packet loss the SCTP data-channel
 * transport tolerates at 8 Mbps before it can't carry the full stream.
 * (SCTP's congestion control is loss-based, unlike WebRTC video's GCC.)
 */
for (const loss of [0, 0.001, 0.003, 0.008]) {
  for (const access of [3, 12]) {
    SCENARIOS.push({
      ...base,
      name: `loss-${(loss * 100).toFixed(1)}pct-rtt${2 * (access + 2 + 1)}`,
      description: `Single viewer, ${(loss * 100).toFixed(1)} % bursty loss, ~${2 * (access + 2 + 1)} ms RTT.`,
      durationS: 25,
      nodes: {
        streamer: { profile: 'giga', capKbps: 100_000 },
        viewer: { profile: 'ftth500', capKbps: 0, link: { loss, loss_burst: 2, access_ms: access } },
      },
    });
  }
}

