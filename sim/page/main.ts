/**
 * Simulation page: one simulated participant, running the real RelayHub over
 * real RTCPeerConnections / data channels. Loaded in a headless Chromium that
 * lives in its own network namespace behind the link emulator.
 *
 * The harness drives it through window.__sim* functions and receives
 * signaling + stats through the exposed window.__out().
 */
import { RelayHub, type Reliability } from '../../client/src/lib/relay/hub';
import { TrackDecoder, TrackEncoder, type RelayCodecConfig } from '../../client/src/lib/relay/codec';

interface SimConfig {
  id: string;
  peers: string[];
  broadcaster: string;
  bitrateKbps: number;
  fps: number;
  audioKbps: number;
  capKbps: number;
  reliability: Reliability;
  strategy: 'tree' | 'star';
  /** Periodic safety keyframe, ms (0 = only on request). */
  keyframeIntervalMs: number;
  /** Temporal layering pattern of the synthetic encoder. */
  svc: 'L1T3' | 'L1T1';
  hubLog: boolean;
  /** 'synthetic' = sized random payloads; 'codec' = real WebCodecs encode/decode of a canvas animation. */
  source: 'synthetic' | 'codec';
  width: number;
  height: number;
  /**
   * 'relay' = the RelayHub. 'mesh' = today's app: the broadcaster sends a
   * WebRTC video track to every viewer (one encoder per viewer, GCC per
   * viewer). Mesh implies the codec source.
   */
  transport: 'relay' | 'mesh';
}

declare global {
  interface Window {
    __out: (kind: string, data: unknown) => Promise<void>;
    __simInit: (cfg: SimConfig) => void;
    __simConnect: () => void;
    __simSignal: (from: string, msg: SignalMsg) => Promise<void>;
    __simStart: () => void;
    __simWatch: () => void;
    __simUnwatch: () => void;
    __simPeerLeft: (id: string) => void;
    __simLeave: () => void;
    __simSetCap: (kbps: number) => void;
  }
}

type SignalMsg = { sdp: RTCSessionDescriptionInit } | { ice: RTCIceCandidateInit | null };

const now = () => performance.timeOrigin + performance.now();
let cfg: SimConfig;
let hub: RelayHub;
const pcs = new Map<string, RTCPeerConnection>();

// ---------- stats accumulated between reports ----------
let latencies: number[] = [];
let lastFrameAt = 0;
let freezeMs = 0;
let freezes = 0;
let frames = 0;
let tlCount = [0, 0, 0];
let hopsSum = 0;
let watching = false;
let firstFrameAt: number | null = null;
let watchAt: number | null = null;

function out(kind: string, data: unknown) {
  void window.__out(kind, data);
}

window.__simInit = (c) => {
  cfg = c;
  hub = new RelayHub(
    {
      myId: c.id,
      uploadCapacityKbps: c.capKbps,
      now,
      reliability: c.reliability,
      strategy: c.strategy,
      log: c.hubLog ? (m) => out('log', m) : undefined,
    },
    {
      onFrame: (_from, f) => {
        if (f.audio) return;
        if (cfg.source === 'codec') {
          // Frames are measured where they are presented (see watchVideo).
          tlCount[f.tl] = (tlCount[f.tl] ?? 0) + 1;
          decodeFrame(f);
          return;
        }
        const t = now();
        frames++;
        tlCount[f.tl] = (tlCount[f.tl] ?? 0) + 1;
        hopsSum += f.hops;
        latencies.push(f.completedAt - f.captureTs);
        if (firstFrameAt === null) {
          firstFrameAt = t;
          out('event', { type: 'first-frame', t, sinceWatchMs: watchAt ? t - watchAt : null });
        }
        if (lastFrameAt) {
          const gap = t - lastFrameAt;
          // WebRTC's freeze definition: gap > max(3 × nominal frame time, nominal + 150 ms).
          const nominal = 1000 / cfg.fps;
          if (gap > Math.max(3 * nominal, nominal + 150)) {
            freezes++;
            freezeMs += gap;
            out('event', { type: 'freeze', t, gapMs: gap });
          }
        }
        lastFrameAt = t;
      },
      onKeyframeRequest: () => {
        forceKey = true;
        codecEnc?.requestKeyframe();
      },
      onPlan: (plan) => {
        out('plan', Object.fromEntries(plan.parent));
      },
    },
  );
  const meshTrack = c.transport === 'mesh' && c.id === c.broadcaster ? stampedCanvasTrack() : null;
  for (const peer of c.peers) {
    const pc = new RTCPeerConnection({ iceServers: [] });
    pcs.set(peer, pc);
    hub.attachPeer(peer, pc); // negotiated channels exist before the offer → SCTP m-line
    pc.onicecandidate = ({ candidate }) => out('signal', { to: peer, msg: { ice: candidate?.toJSON() ?? null } });
    pc.onconnectionstatechange = () => out('pcstate', { peer, state: pc.connectionState });
    if (meshTrack) pc.addTrack(meshTrack);
    if (c.transport === 'mesh' && peer === c.broadcaster) {
      pc.ontrack = ({ track }) => watchVideo(new MediaStream([track]));
    }
  }
  setInterval(report, 1000);
};

/** Who offers: the broadcaster in mesh mode (it owns the video m-lines), else the lower id. */
function isOfferer(peer: string): boolean {
  if (cfg.transport === 'mesh' && (cfg.id === cfg.broadcaster || peer === cfg.broadcaster)) return cfg.id === cfg.broadcaster;
  return cfg.id < peer;
}

/** Second phase (after every page is initialised): offers go out. */
window.__simConnect = () => {
  for (const [peer, pc] of pcs) {
    if (!isOfferer(peer)) continue;
    void (async () => {
      await pc.setLocalDescription(await pc.createOffer());
      out('signal', { to: peer, msg: { sdp: pc.localDescription!.toJSON() } });
    })();
  }
};

window.__simSignal = async (from, msg) => {
  const pc = pcs.get(from);
  if (!pc) return;
  if ('sdp' in msg) {
    await pc.setRemoteDescription(msg.sdp);
    if (msg.sdp.type === 'offer') {
      await pc.setLocalDescription(await pc.createAnswer());
      out('signal', { to: from, msg: { sdp: pc.localDescription!.toJSON() } });
    }
  } else {
    try {
      await pc.addIceCandidate(msg.ice ?? undefined);
    } catch {
      /* end-of-candidates quirks */
    }
  }
};

// ---------- synthetic encoder ----------
let forceKey = false;

/**
 * Frame sizes follow a screen-content encoder at constant bitrate: an L1T3
 * pattern T0 T2 T1 T2 with base-layer frames bigger (longer reference
 * distance), ±25 % noise, and keyframes ≈ 8× an average frame.
 */
function startSource() {
  const avg = (cfg.bitrateKbps * 1000) / 8 / cfg.fps;
  const pattern = cfg.svc === 'L1T3' ? [0, 2, 1, 2] : [0];
  const weight = cfg.svc === 'L1T3' ? [1.5, 1.0, 0.75] : [1];
  const junk = new Uint8Array(4 * 1024 * 1024);
  for (let i = 0; i < junk.length; i += 65536) crypto.getRandomValues(junk.subarray(i, i + 65536));
  let i = 0;
  let lastKey = -Infinity;
  const frameMs = 1000 / cfg.fps;
  const t0 = performance.now();
  const tick = () => {
    const due = Math.floor((performance.now() - t0) / frameMs);
    // If the page stalls, skip frames (like a real capture pipeline) rather than burst.
    if (due > i + 2) i = due;
    while (i <= due) {
      const t = now();
      let key = forceKey || i === 0 || (cfg.keyframeIntervalMs > 0 && t - lastKey > cfg.keyframeIntervalMs);
      const tl = key ? 0 : pattern[i % pattern.length]!;
      const noise = 0.75 + Math.random() * 0.5;
      const size = Math.round(key ? avg * 8 : avg * weight[tl]! * noise);
      const off = Math.floor(Math.random() * (junk.length - size));
      hub.sendFrame({ key, tl, data: junk.subarray(off, off + size), mediaTs: Math.round(i * frameMs * 1000), captureTs: t });
      if (key) {
        lastKey = t;
        forceKey = false;
        key = false;
      }
      i++;
    }
    setTimeout(tick, Math.max(0, t0 + i * frameMs - performance.now()));
  };
  tick();
  // Game audio: 20 ms Opus packets.
  if (cfg.audioKbps > 0) {
    const asize = Math.round((cfg.audioKbps * 1000) / 8 / 50);
    let a = 0;
    setInterval(() => {
      hub.sendFrame({ key: false, audio: true, tl: 0, data: junk.subarray(0, asize), mediaTs: a++ * 20000 });
    }, 20);
  }
}

window.__simStart = () => {
  if (cfg.transport === 'mesh') {
    // Same sender settings the app applies: bitrate cap, keep frame rate.
    for (const pc of pcs.values()) {
      for (const sender of pc.getSenders()) {
        if (sender.track?.kind !== 'video') continue;
        const params = sender.getParameters();
        params.encodings = params.encodings?.length ? params.encodings : [{}];
        for (const e of params.encodings) e.maxBitrate = cfg.bitrateKbps * 1000;
        (params as { degradationPreference?: string }).degradationPreference = 'maintain-framerate';
        void sender.setParameters(params).catch(() => {});
      }
    }
    return;
  }
  if (cfg.source === 'codec') void startCodecSource();
  else {
    hub.startBroadcast(cfg.bitrateKbps, { codec: 'synthetic' });
    startSource();
  }
};

// ---------- real codec path ----------
let decoder: TrackDecoder | null = null;
let codecEnc: TrackEncoder | null = null;
let encoderInfo: RelayCodecConfig | null = null;

/** A moving scene (scrolling gradient + bouncing box + frame counter) on a canvas. */
async function startCodecSource() {
  const track = stampedCanvasTrack();
  const enc = new TrackEncoder(track, { fps: cfg.fps, bitrateKbps: cfg.bitrateKbps, now }, (f) =>
    hub.sendFrame({ key: f.key, tl: f.tl, data: f.data, mediaTs: f.mediaTs, captureTs: f.captureTs }),
  );
  encoderInfo = await enc.start();
  codecEnc = enc;
  out('event', { type: 'encoder', info: encoderInfo });
  hub.startBroadcast(cfg.bitrateKbps, encoderInfo);
}

function decodeFrame(f: { key: boolean; data: Uint8Array; mediaTs: number }) {
  if (!decoder) {
    const info = hub.streams().find((s) => s.broadcasterId === cfg.broadcaster)?.config as RelayCodecConfig | undefined;
    if (!info) return;
    decoder = new TrackDecoder(info, () => {});
    watchVideo(decoder.stream);
  }
  decoder.decode(f);
}

// ---------- glass-to-glass measurement for real video ----------
// The broadcaster paints its wall clock into the top rows of every frame as
// 32 black/white blocks; viewers read it back from each *presented* frame.
// That measures capture → encode → network/relays → decode → display, the
// same way for the WebRTC mesh and the relay path.
const STAMP_BLOCKS = 34; // 2 marker blocks + 32 bits
const STAMP_H = 24;

function stampedCanvasTrack(): MediaStreamTrack {
  const canvas = document.createElement('canvas');
  canvas.width = cfg.width;
  canvas.height = cfg.height;
  const ctx = canvas.getContext('2d')!;
  let n = 0;
  const draw = () => {
    const w = canvas.width;
    const h = canvas.height;
    const g = ctx.createLinearGradient((n * 7) % w, 0, w, h);
    g.addColorStop(0, `hsl(${n % 360},70%,40%)`);
    g.addColorStop(1, `hsl(${(n + 180) % 360},70%,20%)`);
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, w, h);
    ctx.fillStyle = '#fff';
    ctx.fillRect(((n * 11) % (w - 100)) | 0, (h / 2 + Math.sin(n / 10) * h * 0.3) | 0, 100, 100);
    ctx.font = '48px monospace';
    ctx.fillText(String(n), 20, 90);
    const stamp = Math.floor(now()) >>> 0;
    const bw = w / STAMP_BLOCKS;
    for (let i = 0; i < STAMP_BLOCKS; i++) {
      const white = i === 0 ? true : i === 1 ? false : ((stamp >>> (i - 2)) & 1) === 1;
      ctx.fillStyle = white ? '#fff' : '#000';
      ctx.fillRect(Math.floor(i * bw), 0, Math.ceil(bw), STAMP_H);
    }
    n++;
  };
  draw();
  setInterval(draw, 1000 / cfg.fps);
  const track = canvas.captureStream(cfg.fps).getVideoTracks()[0]!;
  track.contentHint = 'motion';
  return track;
}

let presentedHeight = 0;

function watchVideo(stream: MediaStream) {
  const video = document.createElement('video');
  video.muted = true;
  video.autoplay = true;
  video.playsInline = true;
  video.width = 320;
  video.srcObject = stream;
  document.body.append(video);
  void video.play().catch(() => {});
  const probe = document.createElement('canvas');
  const pctx = probe.getContext('2d', { willReadFrequently: true })!;
  const onFrame = () => {
    const vw = video.videoWidth;
    const vh = video.videoHeight;
    if (vw > 0 && vh > 0) {
      const sh = Math.max(1, Math.round((STAMP_H * vh) / cfg.height));
      probe.width = vw;
      probe.height = sh;
      pctx.drawImage(video, 0, 0, vw, sh, 0, 0, vw, sh);
      const px = pctx.getImageData(0, 0, vw, sh).data;
      const y = Math.floor(sh / 2);
      const bit = (i: number) => {
        const x = Math.floor(((i + 0.5) * vw) / STAMP_BLOCKS);
        const o = (y * vw + x) * 4;
        return px[o]! + px[o + 1]! + px[o + 2]! > 384;
      };
      const t = now();
      if (bit(0) && !bit(1)) {
        let v = 0;
        for (let i = 0; i < 32; i++) if (bit(i + 2)) v |= 1 << i;
        const lat = ((Math.floor(t) >>> 0) - (v >>> 0) + 2 ** 32) % 2 ** 32;
        if (lat < 10_000) latencies.push(lat);
      }
      presentedHeight = vh;
      frames++;
      if (firstFrameAt === null) {
        firstFrameAt = t;
        out('event', { type: 'first-frame', t, sinceWatchMs: watchAt ? t - watchAt : null });
      }
      if (lastFrameAt) {
        const gap = t - lastFrameAt;
        const nominal = 1000 / cfg.fps;
        if (gap > Math.max(3 * nominal, nominal + 150)) {
          freezes++;
          freezeMs += gap;
          out('event', { type: 'freeze', t, gapMs: gap });
        }
      }
      lastFrameAt = t;
    }
    video.requestVideoFrameCallback(onFrame);
  };
  video.requestVideoFrameCallback(onFrame);
}

/** Mesh sender state: why WebRTC limited each viewer's stream, and at what size. */
async function meshSenderStats() {
  const limited: Record<string, number> = {};
  const heights: number[] = [];
  let encodeMsPerFrame = 0;
  let n = 0;
  for (const pc of pcs.values()) {
    const rep = await pc.getStats();
    rep.forEach((s) => {
      if (s.type === 'outbound-rtp' && s.kind === 'video') {
        const r = s.qualityLimitationReason ?? 'none';
        limited[r] = (limited[r] ?? 0) + 1;
        if (s.frameHeight) heights.push(s.frameHeight);
        if (s.framesEncoded) {
          encodeMsPerFrame += (s.totalEncodeTime / s.framesEncoded) * 1000;
          n++;
        }
      }
    });
  }
  return { limited, heights, encodeMsPerFrame: n ? encodeMsPerFrame / n : null };
}

window.__simWatch = () => {
  watching = true;
  watchAt = now();
  if (cfg.transport === 'mesh') return; // the mesh sends to everyone anyway
  firstFrameAt = null;
  const tryWatch = () => {
    if (hub.streams().some((s) => s.broadcasterId === cfg.broadcaster)) hub.watch(cfg.broadcaster);
    else setTimeout(tryWatch, 100);
  };
  tryWatch();
};

window.__simUnwatch = () => {
  watching = false;
  hub.unwatch(cfg.broadcaster);
};

window.__simPeerLeft = (id) => {
  hub.detachPeer(id);
  pcs.get(id)?.close();
  pcs.delete(id);
};

window.__simLeave = () => {
  hub.dispose();
  for (const pc of pcs.values()) pc.close();
  pcs.clear();
};

window.__simSetCap = (kbps) => hub.setUploadCapacity(kbps);

function pct(xs: number[], p: number): number | null {
  if (xs.length === 0) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(p * s.length))]!;
}

function report() {
  void reportAsync();
}

async function reportAsync() {
  const st = hub.stats();
  const mesh = cfg.transport === 'mesh' && cfg.id === cfg.broadcaster ? await meshSenderStats() : null;
  const t = now();
  // A viewer that is watching but receiving nothing is frozen right now.
  let ongoingFreeze = 0;
  if (watching && lastFrameAt && t - lastFrameAt > 250) ongoingFreeze = t - lastFrameAt;
  out('stats', {
    t,
    watching,
    frames,
    fpsByTl: tlCount,
    latP50: pct(latencies, 0.5),
    latP95: pct(latencies, 0.95),
    latMax: latencies.length ? Math.max(...latencies) : null,
    hopsAvg: frames ? hopsSum / frames : null,
    freezes,
    freezeMs,
    ongoingFreeze,
    decoded: decoder?.decoded ?? null,
    presentedHeight: presentedHeight || null,
    mesh,
    decodeErrors: decoder?.errors ?? null,
    hub: st,
  });
  latencies = [];
  frames = 0;
  tlCount = [0, 0, 0];
  hopsSum = 0;
  freezes = 0;
  freezeMs = 0;
}
